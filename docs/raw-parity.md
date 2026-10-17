# RAW parity: cameras, formats, demosaic, colour and lens profiles

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-16 · **Change:** minor (Kelvin white balance for Canon ColorData presets and the Nikon Z5II; upstream: integration batch 36 and the camera-coverage sweep #1327, 545 models verified upstream; re-measured after the merge: 596 verified; Canon EOS R6 verified: 315 verified, use-weighted ~90%; Canon CRW decoded, 15 bodies verified; Epson ERF and Mamiya MEF decoded, 4 bodies verified: 314 verified, use-weighted ~90%; compressed ORF decoded: E-M1 and E-M10 Mark III verified; use-weighted 85% → 87%; compressed Samsung SRW 32770 / 32772 / 32773 decoded, 7 bodies checked; Minolta MRW decoded, 6 bodies verified; ARW version 1 and the Z 6II's packed NEF decode, 31 popular bodies verified: use-weighted 87% → 89%, 57% on verified bodies; Z 8 and Z 9 lossless verified: 280 verified, 59% on verified bodies; pre-X FinePix SuperCCD RAF decoded (`raf_superccd.rs`), 15 FinePix bodies verified: 295 verified, Fujifilm 93% → 99%, 60% on verified bodies) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6 (Camera Raw 18.7)

Raw processing is the deepest single dimension of Lightroom parity: a photographer whose camera
does not decode, or decodes with the wrong colour, cannot switch. This document measures it model by
model. The numbers feed [target-app-parity.md](target-app-parity.md); individual shortfalls are
ranked in [gaps.md](gaps.md). Details per format: [cr3.md](cr3.md),
[raf-compression.md](raf-compression.md), [sony-lens-corrections.md](sony-lens-corrections.md),
[sony-ilce7rm6-lens-corrections.md](sony-ilce7rm6-lens-corrections.md), [nikon-lens-corrections.md](nikon-lens-corrections.md),
[camera-preview-colour.md](camera-preview-colour.md), [denoise.md](denoise.md).

## Headline

| Measure | Value | Kind |
|---|---:|---|
| Adobe-listed camera models | **1,446** | measured: [raw/adobe-cameras.tsv](raw/adobe-cameras.tsv) |
| Decoded from sensor data and **verified** on a real file | **596** (41%) | measured: [raw/evidence.tsv](raw/evidence.tsv) |
| Decoder path should handle it, **unverified** | **724** (50%) | measured from the format rules (632 of them DNG, mostly phones) |
| **Preview-only** (embedded JPEG) | **89** (6%) | measured |
| **Unsupported** (not imported, refused or decodes wrongly) | **37** (3%) | measured |
| Models added since 2017 (Camera Raw 10+): verified / unverified / preview / unsupported | 184 / 557 / 5 / 8 of 754 | measured |
| **Share of photographers whose camera fully works** (decodes from sensor data), use-weighted | **~98%** | estimated: measured classes × written usage weights × per-format confidence |
| …of which on a body we have verified | **~87%** | estimated, same weights |
| Camera colour: Adobe profiles vs ours | 1,500 Adobe Standard profiles, 481 models with Camera Matching profiles vs **11 bundled profiles** + per-file fits + 52 spectral matrices | measured (bundle listing, Adobe list, `assets/camera-profiles`) |
| Lens profiles: Adobe vs ours | **3,635 vs 0** (in-file corrections only) | measured |
| **RAW readiness** (decoding 60%, colour 25%, lens 15%) | **~63%** (range 57–67%) | estimated, see [Scores](#scores) |

Re-measure with `python3 docs/raw/coverage.py` (standard library only): it classifies every Adobe
model from the format rules in the script (which mirror `crates/raw/src`) plus the per-model evidence,
rewrites [raw/coverage.tsv](raw/coverage.tsv) and prints the tables below; `--check` fails when the
committed table is stale.

## How it was measured

- **Adobe's list**: helpx "Camera Raw supported cameras" (the live page refuses scripted requests, so
  the Wayback Machine capture of 2026-10-03 was parsed): 1,446 camera rows from 40 makers after
  dropping profile-only rows; maker, model, extension(s), Camera Matching profile, minimum Camera Raw
  version. The year a model was added is approximated from that Camera Raw major version.
- **What we decode**: the decoders and their refusals in `crates/raw/src` (`lib.rs` routing, `vendor/*.rs`
  coding variants, `crx.rs` CRX limits, `srw.rs` compressed refusals), the import list
  `lcc_engine::import::EXTENSIONS`, and `KNOWN_UNSUPPORTED` in `crates/raw/tests/corpus.rs`. Doc
  claims were not used.
- **Verified** means a real file from that body decoded from sensor data in: the corpus test over the
  `RAW_SAMPLES` manifest (`xtask/src/main.rs`, about 500 CC0 files after the upstream merge; bodies whose sample is in
  `KNOWN_UNSUPPORTED` excluded), `crates/raw/tests/cr3_corpus.rs` (3 bodies exact against an external decoder;
  the EOS R5 Mark II pinned, with a QP-map check of 33,665 private files; 22 more bodies pinned on one CC0
  file each and checked against the camera JPEG, [cr3.md](cr3.md#further-bodies-one-cc0-file-each)), the RAF reference
  check ([raf-compression.md](raf-compression.md)), and the raw.pixls.us bulk checks recorded in issue
  #535 (all 100 Sony bodies there; 87 decode) and PR #215 (every Panasonic / Leica RW2 / RWL / RAW body
  there, 178 files from 118 bodies), and a recorded check of 38 private Leica Q3 DNGs (2026-10-11: every file
  decoded and developed at full size, 9520 × 6336 and 6336 × 9520; mean CIE76 ΔE to the camera JPEG 6.3–13.9
  at 400 px; the files are not committed). Bulk-check body lists were matched to Adobe names through the
  raw.pixls.us folder names. Checks without a body list (≈ 60 NEFs, 35 CR2 bodies for the CFA layout)
  are not counted as verified.
- **2026-10-12 public sweep:** 30 further models on 40 CC0 files, independently downloaded and
  decoded on main `25ef65c8`; [raw/coverage-sweep.tsv](raw/coverage-sweep.tsv) records the URLs,
  SHA-256, dimensions, FNV-1a sensor hashes and render/reference luminance correlations (0.6801
  to 0.9998). The hashes pin the app's own output. The regression test is
  `crates/raw/tests/coverage_sweep_corpus.rs`; downloads are in `RAW_SAMPLES`. Ten references are
  embedded TIFF images; the rest are JPEGs. FFF TIFF previews are not display-ready and check
  image structure only. Eleven candidate models were excluded for identity, provenance or
  missing/mismatched reference images. D6 / Z 6 / Z 7 verification covers compressed NEF only;
  uncompressed remains preview-only (#671), as do Z 8 HE / HE* files. Own-matrix phone, drone
  and other DNGs can decode correctly while rendering darker than their camera JPEG (#1277).
  Verified means sensor data decoded, not colour parity.
- **Usage weights** ([raw/usage-weights.tsv](raw/usage-weights.tsv)): maker buckets from 2025 market
  data (Nikkei production: Canon ~36%, Sony ~29%, Nikon ~14%, Fujifilm ~12% of mirrorless; BCN retail:
  Sony 29.9%, Canon 27.4%, Nikon 15.1%), adjusted for the DSLR installed base and for the makers named
  in this repo's 325 user issues (Sony 33, Nikon 31, Canon 26, phones 23, Fujifilm 9, Panasonic / Leica
  8, OM / Olympus 6, Pentax / Ricoh 6). Inside each bucket, 80% goes to the popular bodies of the last
  ~8 years (listed in the file), 20% to the bucket's other Adobe models.
- **Per-format confidence** for unverified bodies (the chance their files decode correctly): DNG, CR2,
  RAF, RW2 0.95; NEF, ARW, PEF 0.9; ORF, SRW, SR2 0.8; **CR3 0.6** (the CRX decoder refuses several
  coding variants; 26 bodies are verified, every CC0 CR3 on raw.pixls.us decodes).

## By maker (measured)

| Maker | Adobe models | verified | unverified | preview | unsupported | Main reason for preview / unsupported |
|---|---:|---:|---:|---:|---:|---|
| Sony | 149 | 104 | 43 | 0 | 2 | SRF (DSC-F828, DSC-V3): encrypted four-colour RGBE data, not decoded |
| Canon | 134 | 110 | 16 | 6 | 2 | CRW decodes since 2026-10-13 (15 Bayer bodies verified); the CMYG CRW bodies (G1, Pro70, Pro90 IS, 600, A5, A50) preview; EOS-1D / 1Ds TIFF raws not imported |
| Samsung | 128 | 14 | 114 | 0 | 0 | compressed SRW decodes (NX300, NX500, NX2000, NX3000 verified; NX1, NX30, NX mini checked too but not in Adobe's list as such) |
| Apple | 113 | 4 | 109 | 0 | 0 | — (DNG / ProRAW) |
| Panasonic | 104 | 93 | 11 | 0 | 0 | — |
| Nikon | 99 | 77 | 19 | 0 | 3 | COOLPIX 5700 / 8400 / 8800 decode wrongly (two stacked half-height fields, CMYG read as RGB); HE / HE★ is a per-shot setting, preview-only |
| Fujifilm | 97 | 78 | 17 | 2 | 0 | S200 / S205 EXR (upright EXR array without a raw IFD) preview-only; SuperCCD lattices decode since 2026-10-13 |
| Google | 88 | 5 | 83 | 0 | 0 | — |
| Olympus | 71 | 19 | 52 | 0 | 0 | — (compressed ORF decodes since 2026-10-13; 2 bodies verified) |
| Leica | 61 | 27 | 34 | 0 | 0 | — |
| Xiaomi, Oppo, LG, Huawei, OnePlus, Motorola, Nokia | 139 | 0 | 139 | 0 | 0 | — (DNG) |
| Pentax / Ricoh | 60 | 41 | 19 | 0 | 0 | — |
| Hasselblad | 34 | 7 | 2 | 25 | 0 | Hasselblad-compressed 3FR / FFF |
| Phase One / Leaf / Mamiya | 59 | 1 | 0 | 43 | 15 | IIQ, MOS preview; TIFF raws not imported; MEF decodes (Mamiya ZD verified) |
| Casio | 26 | 0 | 26 | 0 | 0 | — (DNG) |
| DJI | 21 | 4 | 17 | 0 | 0 | — (DNG) |
| OM Digital Solutions | 7 | 0 | 7 | 0 | 0 | — (compressed ORF, unverified) |
| Kodak, Konica Minolta, Epson, Sigma, GoPro, Contax, others | 56 | 12 | 16 | 13 | 15 | DCR, GPR not imported; KDC, X3F preview (MRW decodes since 2026-10-13: DiMAGE 7, 7Hi, A1, A2, A200, Dynax 7D verified; ERF: R-D1, R-D1s, R-D1x verified) |
| **All** | **1,446** | **596 (41%)** | **724 (50%)** | **89 (6%)** | **37 (3%)** | |

## By use (estimated)

| Bucket | Weight | Camera fully works | On a verified body | Why |
|---|---:|---:|---:|---|
| Canon | 33% | 98% | 96% | 18 of the 22 popular bodies write CR3; 26 CR3 bodies verified (raw.pixls.us has a CC0 file for each), the rest at 0.6; CRW decodes |
| Sony | 27% | 99% | 93% | bulk-checked; ARW version 1 (A100, A200…A390) decodes |
| Nikon | 16% | 98% | 89% | decoders cover every NEF coding but HE; 77 bodies verified |
| Fujifilm | 10% | 99% | 95% | X100V, X100VI, X-E4, X-S10, X-S20, X-T30 II added to the verified bodies; pre-X SuperCCD FinePix decode (12 bodies verified) |
| Panasonic | 4% | 100% | 98% | bulk-checked |
| OM / Olympus | 3% | 81% | 6% | compressed ORF decodes; 2 of the popular bodies' predecessors verified, the rest at 0.8 |
| Phones | 3% | 95% | 5% | DNG |
| Pentax / Ricoh | 1.5% | 100% | 93% | K-1 II, K-3 III, K-70, KF verified |
| Leica | 1% | 99% | 72% | the Q3 verified, 13 more bodies pinned on CC0 files |
| DJI | 1% | 96% | 19% |  |
| Medium format and others | 0.5% | 35% | 11% | IIQ, 3FR, X3F; MRW, ERF, MEF decode |
| **Weighted** | 100% | **98%** | **87%** | |

Nearly every raw photographer's camera decodes (98%), and about 7 in 8 use a body we have actually
checked (the per-model CC0 sweeps of October 2026); OM / Olympus bodies, which decode but are barely verified, are the
largest gap left.

## Variant matrix (depth per format)

✅ decoded and checked on real files · 🟡 decoded, partial or unverified · ⬜ preview-only or refused.

| Format | Variant | Status | Evidence |
|---|---|---|---|
| 2026-10-12 | minor | Mamiya MEF decoded (`vendor/mef.rs`): ZD verified; 241 verified, 35 unsupported |
| 2026-10-12 | minor | Epson ERF decoded (`vendor/erf.rs`): R-D1, R-D1s, R-D1x verified; 240 verified, 229 preview (merged after the Leica Q3 and Hasselblad Lunar; duplicated summary rows removed) |
| CR2 | lossless JPEG slices | ✅ | 8 corpus bodies; CFA phase from `CR2CFAPattern` (#85, 35 bodies); masked-column black level; white level from the active area only (#1272: masked rows at 16383 hid the image plateau on the EOS 50D, 60D, 450D (Rebel XSi), 500D (Rebel T1i), 550D (Rebel T2i), 1200D (Rebel T5) and 100D (Rebel SL1), clipped highlights at 81–98%) |
| CR2 | sRAW / mRAW (YCbCr) | ✅ | `vendor/cr2/sraw.rs`, 31 samples from 16 models |
| CR3 | CRX lossless, version 0x100 | ✅ | M50, R100, R8 exact against a reference; R5 Mark II (odd-height planes) pinned |
| CR3 | C-RAW 0x100 (horizontal tiles), 0x200 (single 14-bit tile, adaptive QP incl. the QP-map escape) | ✅ | same 3 bodies exact; R5 Mark II pinned, QP maps of 33,665 files decode exactly, matches the camera JPEG |
| CR3 | C-RAW 0x100 vertical / grid tiles | 🟡 | decodes (same 5/3 tile-overlap rule as the verified horizontal tiles); synthetic tests only, no real file seen |
| CR3 | other CRX variants (partial subbands, other plane flags, 0x200 multi-tile) | ⬜ | refused in `crx.rs`, falls back to the preview; which bodies write them is unmeasured |
| CR3 | sRAW / mRAW | ⬜ | 4-plane Bayer only |
| CR3 | HEIF / HDR PQ | ⬜ | HEVC tracks recognised, not decoded |
| CR3 | dual-pixel raw | ⬜ | never substituted |
| CRW | CIFF, Huffman blocks, 10-bit and 12-bit with low bits | ✅ | `vendor/crw.rs`; EOS D30, D60, 10D, 300D, PowerShot G2, G3, G5, G6, Pro1, S30, S40, S45, S50, S60, S70 match the camera JPEG (colour agreement 0.77–0.99); as-shot WB from `0x10a9` / `0x102c` / `0x0032` or measured from the JPEG; CMYG bodies (G1, Pro90 IS, Pro70, 600, A5, A50) refused, preview |
| NEF | uncompressed, lossless, lossy type 1 / 2, lossy after split | ✅ | D5100, D7000, D7500, Z f corpus; ~60 NEFs by hand; #633; D4, D90, D5000, D600 and D610 image area measured against the camera JPEG (white level too for the D4, D90 and D5000) |
| NEF | packed 12 / 14-bit stored under a compressed tag without a linearization table (Z 6II "Packed 14 bits") | ✅ | #769; `nef.rs` `packed_lsb` (rows tight or padded to 16 bytes, the layouts of upstream PR #671); Z 6II CC0 file in the corpus |
| NEF | High Efficiency / HE★ (JPEG XS) | 🟡 | verified profile decodes (Z50 II HE / HE★, `vendor/nefhe.rs`, `docs/nikon-he.md`); other profiles named, preview (#193) |
| ARW | uncompressed, cRAW (ARW2), lossless LJ92 (L / M / S) | ✅ | 86 bodies (#535); M/S black level 1024 |
| ARW | packed 12-bit (A900, A850) | ✅ | one CC0 file each |
| ARW | ARW6 Compressed / HQ (compression 32766; ILCE-7RM6) | ✅ | 8 CC0 files; 12-bit mode unsupported; `docs/arw6-compression.md` |
| ARW | ARW version 1: 12-bit Huffman-coded columns of the DSLR-A100 and A200…A390 | ✅ | `arw1.rs`; one CC0 file per body (9) in the corpus, black 0; the A100's frame and WB from its MRI block |
| ARW | SRF (DSC-F828, V3) | ⬜ | preview only (#702); encrypted 14-bit RGBE (four-colour) data |
| ARW | ARQ pixel shift | ⬜ | no code |
| SR2 | DSC-R1 | ✅ | CC0 3221 sample |
| RAF | uncompressed, lossless and lossy compressed; Bayer and X-Trans; GFX 14/16-bit | ✅ | 24 files from 13 bodies exact |
| RAF | pre-X FinePix SuperCCD without a raw IFD (45° lattice; SR Pro S frame; SR S photosites) | ✅ | `raf_superccd.rs`: 15 CC0 files from 14 bodies (E550, E900, F700, S2Pro, S3Pro, S5Pro, S5000, S5200, S5600, S6000fd, S6500fd, S7000, S9500, S9600, S100FS) checked against the camera JPEG; the SR bodies' R photosites (highlight range) are not used yet |
| RAF | SuperCCD EXR stored upright without a raw IFD (S200 / S205 EXR) | ⬜ | refused (no Bayer phase matches the camera JPEG: the EXR array) |
| ORF | uncompressed 16-bit, 12-bit packed, two-field 12-bit, 12.8 bpp | ✅ | E-1, E-400, XZ-2 corpus; #651 |
| ORF | **compressed** (every Olympus / OM body since ~2008) | 🟡 | `orf.rs` `decode_compressed`; E-M1 and E-M10 III corpus files decode and render correctly; OM-1 / OM-5 and the other ~70 bodies unverified |
| RW2 / RWL / RAW | formats 2, 4, 5, 6, 7, 8 and 16-bit words | ✅ | 118 bodies (#215) |
| PEF | uncompressed, Huffman, packed 12-bit | ✅ | K10D, K-3, K-5 IIs; files without area tags lose their masked, empty and saturated border lines (#1271: KP, KF, K-3 Mark III, K-70, K-1, K-1 Mark II, K-S1, K-S2, 645Z) |
| PEF / DNG | Pentax pixel shift | ⬜ | frames not combined |
| SRW | uncompressed, 12-bit packed | ✅ | EX1 corpus; NX10, NX20 |
| SRW | compressed 32770 / 32772 / 32773 | ✅ | `srwc.rs`; NX300, NX2000, NX30, NX3000, NX mini, NX1, NX500 match the camera JPEG (colour agreement 0.88–0.98); CFA layout from the embedded JPEG (the files state wrong ones) |
| DNG | uncompressed, LJ92, lossy JPEG / Smart Preview, Deflate incl. float, LinearRaw, tiles / strips, row / column interleave | ✅ | 6 corpus DNGs; 4 ILCE-6400 JXL DNGs |
| DNG | JPEG XL (DNG 1.7, lossless and lossy) | ✅ | synthetic bit-exact; real lossless and lossy files |
| DNG | Apple ProRAW (incl. semantic mattes) | ✅ | iPhone 12 Pro corpus; ProfileGainTableMap read but off (ΔE00 19 against Lightroom with it on) |
| DNG | phone DNGs (Pixel, Galaxy, Xiaomi…) | 🟡 | Pixel 2 XL corpus; 13 further Apple, Google and Samsung models in the pinned sweep; white-level pile-up fix #548 |
| DNG | GoPro GPR (VC-5) | ⬜ | refused |
| 3FR / FFF | DNG-like layouts | 🟡 | six Hasselblad models verified on nine files; FFF neutral-matrix colour remains unverified |
| 3FR / FFF | Hasselblad compression | ⬜ | preview |
| MRW | 16-bit words, 12-bit packing | ✅ | `vendor/mrw.rs`; DiMAGE 7, 7Hi, A1, A2, A200 (GBRG), Dynax / Maxxum 7D match the camera JPEG (colour agreement 0.73–0.99); as-shot WB from `\0WBG` |
| ERF | 12-bit 16-byte blocks | ✅ | `vendor/erf.rs`; R-D1, R-D1s, R-D1x; black from the maker note, WB measured from the preview (port of upstream #1087) |
| MEF | uncompressed 12-bit | ✅ | `vendor/mef.rs`; Mamiya ZD; crop is the preview window, WB from the RGB preview (port of upstream #1105) |
| IIQ, MOS, KDC, X3F | all | ⬜ | preview; an uncompressed MOS, whose IFD0 is the mosaic itself with no Make tag, is recognised by Leaf's data block `0x8606` (#1275) |
| DCR, MFW, DXO, GPR, TIFF raws | all | ⬜ | not imported |

### Compressed ORF: a clean-room starting point

Decoded since 2026-10-13 (`decode_compressed` in `crates/raw/src/vendor/orf.rs`, fork-only under the decoder
reference policy); kept for the method's background.

Two Olympus patents by Takashi Ishikawa describe the *method* of lossless RAW compression in prose; neither names ORF
and neither gives the file layout. See the closed #240 and the open #360 for why a decoder has to be derived from the
files and prose rather than from dcraw-family code.

- [US7333035B2](https://patents.google.com/patent/US7333035): lossless RAW compression for SLR cameras:
  median-edge-detector prediction from the left, upper and upper-left neighbours, a separately coded sign, Golomb-Rice
  codes whose parameter `k` comes from flatness run counters, counters reset per line.
- [US7715639B2](https://patents.google.com/patent/US7715639B2/en): a sibling with predictor variants, `k` derived from
  neighbouring positions, raw low-order bits and a correction filter.

They give the algorithm's shape; constants, the bit layout and the stream header still have to be established from CC0
sample files. A pointer for contributors, not legal advice.

## Demosaic, levels, crops and lens metadata

| Stage | Target | The app | Status |
|---|---|---|---|
| Bayer demosaic | Adobe's own, plus AI "Raw Details" | AHD (default), PPG, bilinear (`crates/raw/src/demosaic`) | 🟡 quality unmeasured against Lightroom; no Raw Details |
| X-Trans demosaic | ✅ | in-house edge-weighted directional | 🟡 unmeasured |
| Foveon | ✅ (older bodies) | — | ⬜ |
| Monochrome / LinearRaw | ✅ | handled, no demosaic | ✅ |
| Pixel shift (Sony ARQ, Pentax, OM high-res, Nikon) | ✅ (Sony, Pentax, Nikon) | — | ⬜ |
| Black and white levels | per model, calibrated | read from each file (masked columns, maker notes, SR2SubIFD, CMP1) | 🟡 per-body bugs keep surfacing: A450–A700 SR2 black level and M/S ARW pedestal (#535, fixed); 5D Mark IV CR2 renders dark (#455, open); white-level pile-up in phone DNGs (#548, fixed); CR2 white level taken from saturated masked rows (#1272, fixed) |
| Crop masks and aspect | ✅ | DefaultCrop, Sony recorded size, Canon AspectInfo, Panasonic in-camera crops, Nikon CropArea | 🟡 7RM3 / 7RM4 crop origin 8–32 px off (cosmetic) |
| Black and white levels | per model, calibrated | read from each file (masked columns, maker notes, SR2SubIFD, CMP1) | 🟡 per-body bugs keep surfacing: A450–A700 SR2 black level and M/S ARW pedestal (#535, fixed); 5D Mark IV CR2 renders dark (#455, open); white-level pile-up in phone DNGs (#548, fixed) |
| Crop masks and aspect | ✅ | DefaultCrop, Sony recorded size, Canon AspectInfo, Panasonic in-camera crops, Nikon CropArea, measured Nikon bodies without it (D4, D90, D5000, D600, D610) | 🟡 7RM3 / 7RM4 crop origin 8–32 px off (cosmetic) |
| Lens metadata | ✅ read for profile selection | EXIF lens fields read; used only for the in-file corrections below | 🟡 |
| Highlight reconstruction | ✅ | neutral clip, chromaticity diffusion (`highlight.rs`) | 🟡 tuned by eye |
| AI denoise | ✅ Bayer and X-Trans, model bundled | Bayer only, user-installed weights, pure-Rust CPU / GPU | 🟡 no X-Trans, no shippable weights |
| Super Resolution | ✅ | — | ⬜ |
| HDR / panorama merge to raw DNG | ✅ | linear DNG (float16 HDR) with the source's colour tags | ✅ |
| Convert to DNG | ✅ | uncompressed or LJ92 tiles (`dngwrite.rs`) | ✅ (no lossy DNG) |

## Colour

| Item | Adobe | The app |
|---|---|---|
| Default profile per model | Adobe Standard for every model (1,500 profile names in the bundle) | the DNG's own matrices; else a pooled profile (**11 bundled**: ILCA-99M2, ILCE-7CR, ILCE-7M4, X-H2S, X-T4, NIKON Z 6_2, Z 8, D5300, D750, D780, D90); else a guarded per-file fit to the camera's embedded JPEG; else spectral matrices (**52 models**, rawtoaces-data); else a neutral matrix |
| Camera Matching looks | 481 models (plus phone-maker looks) | none (the per-file JPEG fit imitates the camera's own look) |
| Creative profiles | Adobe Color, Portrait, Landscape, Vivid, Monochrome… | our own looks (Color, Neutral, Vivid, Landscape, Portrait, Monochrome) |
| DCP / dual-illuminant model | ✅ | dual-illuminant DNG colour (ColorMatrix / ForwardMatrix 1-2, CameraCalibration, AnalogBalance), HueSatMap, LookTable, ProfileToneCurve; external `.dcp` never loaded (clean-room) |
| White balance in Kelvin | ✅ for every model | absolute Kelvin where the camera has colour matrices (its own, or one of the 52 spectral rows), also for files whose look is a per-file fit (#730), or where the camera's white-balance presets in the file give the scale (6 Sony bodies and the Canon PowerShot G1 X Mark III, `kelvin_presets`); other raws keep the relative scale (#296) |

**How far the default rendering is from Adobe's** (CIE ΔE; measured where stated):

- Against the camera JPEG (what the fit targets): the Sony bulk check (#535) accepted fits on 81 of 87
  bodies, median ΔE 5.1, ΔL\* +0.2; rejected fits fall back to a neutral matrix at median ΔE 18.5,
  ΔL\* −11, chroma ×0.39. RW2 140 of 174 files accepted, median 5.0. NEF 13 of 13, 2.9–7.8. RAF 23 of
  24, pooled X-H2S 1.62 / X-T4 2.34. SRW 2.2–7.6.
- Against Lightroom itself (few): Sony DRO median ΔE76 7.5 → 3.3; Olympus E-1 vs Lightroom Classic
  ΔE00 11.25 → 8.30; ProRAW with its gain map ΔE00 19 (why the map is off). No measurement against
  Adobe Standard across models exists.
- User evidence: 33 open issues about raw colour or rendering (wrong colours, dark, flat, green,
  desaturated), the largest group of open core-path issues.

**Colour score ~35%** (estimated): most decoded files start close to the camera's own look (ΔE ~5),
about 10% start far off (ΔE ~18), nothing is calibrated, Kelvin WB is missing for fitted cameras without colour matrices, and
the distance to Adobe Standard is unmeasured. In the readiness numbers this becomes a colour factor
of ×0.85 for a working photographer (they correct colour by hand, at a cost) and ×0.75 for the
essentials user (the default look is what they keep).

## Lens profiles

| Item | Adobe | The app |
|---|---|---|
| Profile database | 3,635 profile files, 56 makers, auto-selected | **0** (Adobe LCPs never used; no database of our own) |
| In-file corrections | ✅ | DNG opcodes (WarpRectilinear, FixVignetteRadial, GainMap…) applied; Panasonic / Leica RW2 distortion (79 photos, 13 bodies, 11 lenses); Sony tag 0x7037 distortion for the ILCE-7RM4A (2 lenses), full-frame Bayer 3:2 ILCE-7CR files with correction Auto or forced Auto (3 lenses; see sony-lens-corrections.md for residuals and limits) and the ILCE-7RM6 (3 lenses, 54 photos; `sony-ilce7rm6-lens-corrections.md`); Nikon NikonNEFInfo distortion (tag `0xc7d5`, about the sensor-shifted optical centre) for the Z 6II and Z f: F-mount lenses on the FTZ within 0.2 px of the camera JPEG, Z lenses within 0.5–1.5 px except the 24-70's wide end (7 px median; `docs/nikon-lens-corrections.md`) |
| Vignetting / lateral CA from maker notes | ✅ | ⬜ |
| Manual distortion, vignetting, CA, defringe | ✅ | ✅ |

**Lens score ~10%** (estimated): automatic correction reaches roughly Panasonic / Leica, phone and
drone DNG users (~9% of photographers by the weights above); Adobe reaches nearly everyone.

## Scores

| Part | Weight | Score | Basis |
|---|---:|---:|---|
| Decoding | 60% | **88%** | use-weighted 98% × 0.9 (88.2%) for per-body decode bugs still surfacing (levels, crops, WB: #535, #455, #461, #702) |
| Colour | 25% | **35%** | see [Colour](#colour) |
| Lens profiles | 15% | **10%** | see [Lens profiles](#lens-profiles) |
| **RAW readiness** | 100% | **~63%** (range 57–67%) | 52.9 + 8.75 + 1.5; demosaic and highlight quality unmeasured |

The previous estimate (earlier on 2026-10-10) was ~50%, with decoding at ~78% weighted by maker
from the format list alone. Measured model by model, decoding comes out about the same; the change is
the written decomposition, not new decoding.

## Remaining work, ranked by photographers unlocked per hour

Hours are Opus 5.5 agent hours, calibrated on this repo's raw PRs: RW2 every format 6.5 h (#215),
compressed RAF 8.6 h (#282), CR3 CRX 10 h (#279), NEF lossy after split 4.7 h (#633), ORF 12.8 bpp
1.7 h (#651), HEIC 7 h (#609). "Gain" is the change in the use-weighted share whose camera fully
works, from [raw/usage-weights.tsv](raw/usage-weights.tsv).

| # | Camera / variant | Gain (points of photographers) | Hours | Gain per hour | Needs |
|---:|---|---:|---:|---:|---|
| 1 | CR3: remaining CRX variants (no public file uses them) | +0.3 | 10–25 | ~0.02 | files that open as a preview, from users |
| 2 | Verify the remaining popular bodies (Canon 90D, 250D, X-H2S, Z 6III; one CC0 file each; Z 8 / Z 9 lossless done 2026-10-13) | +0.3 | 2–4 | ~0.1 | raw.pixls.us has no 90D / 250D / X-H2S / Z 6III sample |
| 5 | ~~Pre-X FinePix SuperCCD RAF~~ (done 2026-10-13); left: S200 / S205 EXR upright EXR array, SR R photosites for highlight range | +0.1 | 3–6 | ~0.02 | the EXR colour layout |
| 6 | Sony DSC-F828 / V3 SRF (encrypted, four-colour RGBE CFA) | < 0.1 | 3–6 | ~0.01 | a four-colour demosaic path |
| 7 | ~~Canon CRW (2000–2004 bodies)~~ (done 2026-10-13; CMYG bodies stay preview) | +1.2 by the uniform tail weight, far less by real use | 0 | — | — |
| 8 | Nikon HE / HE★ NEF (Z 8, Z 9, Z 6III, Z f, Z50II, Z5II set to HE) | +1.1 | 20–40 | ~0.04 | a pure-Rust JPEG XS decoder |
| 9 | ~~Compressed SRW~~, ~~MRW~~, ~~ERF~~, ~~MEF~~ (done 2026-10-13), Hasselblad 3FR, IIQ, X3F, KDC, DCR, GPR | +0.6 together | 45–85 | ~0.01 | samples; low use |

Beyond decoding:

| Gap | Hours | Needs |
|---|---:|---|
| Measured colour calibration for the ~60 popular bodies, Kelvin WB for cameras without matrices, render-fidelity suite against Lightroom | 80–150 | chart shots per camera (a human with cameras) or licensable measured data |
| Lens profile database of our own (fitting tools, data format, top 100 lenses), maker-note vignetting / CA | 40–80 | calibration shots (a human with lenses) |
| Demosaic and highlight quality measured against Lightroom; X-Trans AI denoise | 15–30 | |
| **Total RAW** | **240–465 h** (decoding list above 105–205 h; ~60% parallelizes) | |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-16 | minor | Kelvin white balance from Canon CR2 `ColorData` presets (PowerShot G1 X Mark III) and a built-in preset table for the Nikon Z5II |
| 2026-10-15 | minor | Upstream batch 40 merged: action 3 (verify popular Nikon, Fujifilm, Canon CR2 and Pentax bodies) was already done here and the fork's tables stay ahead; `coverage.py --check` passes, numbers unchanged (596 verified, use-weighted ~98%, 87% on verified bodies) |
| 2026-10-15 | minor | Upstream merge: both sides' evidence combined and re-measured with `docs/raw/coverage.py`: 596 verified, 724 unverified, 89 preview, 37 unsupported; use-weighted ~98% (87% on verified bodies); decoding 88%, RAW readiness ~63% |
| 2026-10-15 | minor | EOS R6 verified (CR3 lossless, 5568x3708, 14-bit; 5 private files checked against DNGLab 0.8.0 reference, exact FNV-1a hashes): 315 verified, 1,001 unverified; use-weighted unchanged at ~90% |
| 2026-10-14 | minor | Camera-coverage sweep (#1327): 30 models verified on 40 CC0 files (`coverage_sweep_corpus.rs`, `coverage-sweep.tsv`; `docs/raw/coverage.py` 515 → 545 verified rows); the counts above are not re-measured here. Sensor verification does not establish colour parity (#1277) |
| 2026-10-13 | minor | Bundled camera profiles 4 → 5 (ILCA-99M2) |
| 2026-10-14 | minor | 21 Sony and Leica models verified (one CC0 raw.pixls.us file each, pinned; `sony_leica_corpus.rs`): 258 verified, 920 unverified; Leica on a verified body 21% → 72%, Sony 91% → 92%, use-weighted 85% and 43% |
| 2026-10-14 | minor | 17 uncompressed Olympus and Samsung models verified on one CC0 file each (`olympus_samsung_corpus.rs`): 254 verified, 932 unverified, 224 preview; OM / Olympus 3% → 6% (5% on verified bodies); use-weighted 85% and 42% unchanged; duplicated summary rows from a merge removed |
| 2026-10-14 | minor | Nikon D600 and D610 image area (6016 × 4016 at 8, 6) measured on 4 CC0 NEFs of each, and the D90's area and white level applied to the D5000 (same sensor and filler, 2 CC0 NEFs): the filler and noise columns on the right no longer develop as a band, and for these bodies the white level is measured inside the image area, issue #1627 |
| 2026-10-11 | minor | Leica Q3 verified by a recorded check of 38 private DNGs (no decode failure; mean ΔE to the camera JPEG 6.3–13.9): 236 verified, 941 unverified, Leica bucket on a verified body 5% → 21%, weighted shares unchanged |
| 2026-10-12 | patch | CR2 white level from the active area, not the saturated masked rows above it (#1272: EOS 50D, 60D, 450D (Rebel XSi), 500D (Rebel T1i), 550D (Rebel T2i), 1200D (Rebel T5) and 100D (Rebel SL1)); scores unchanged |
| 2026-10-12 | minor | Canon PowerShot G9: as-shot white balance from WBInfo (`0x0029`, the row of the preset in ShotInfo), issue #1273; render unchanged (the camera-look fit already absorbed it), As Shot now the camera's; counts unchanged |
| 2026-10-12 | minor | Olympus E-10: as-shot white balance (`0x1017`/`0x1018`) and black level (`0x1012` in `0x102c` ValidBits units) from the main maker note, issue #1273; ΔE76 against its thumbnail 18.5 → 11.5, camera-look fit now accepted; counts unchanged |
| 2026-10-12 | minor | Nikon D2H, D70 and D70s: as-shot white balance from the unenciphered ColorBalance block (`0x0097`), issue #1273 (the D50's is enciphered: not read); renders unchanged (the camera-look fit already absorbed it), As Shot now the camera's; counts unchanged |
| 2026-10-13 | minor | Nikon D90 image area (4288 × 2848 at 10, 10) and white level (3830) measured on 10 of 550 NEFs from one body: its 42 filler columns no longer develop as a pink band |
| 2026-10-12 | minor | Hasselblad Lunar verified (a Sony NEX-7 ARW under Make HASSELBLAD, now decoded): 236 verified, 232 preview; an uncompressed Leaf MOS is recognised as a raw (preview only) instead of opening as a greyscale image of its mosaic (#1275) |
| 2026-10-13 (later) | minor | Canon CRW decoded (`vendor/crw.rs`; 16 CC0 files pinned, 15 Bayer bodies checked against the camera JPEG, CMYG G1 refused); Epson ERF and Mamiya MEF decoded (ports of upstream #1087 / #1105); re-measured: 314 verified, 1,002 unverified, 96 preview, 34 unsupported; use-weighted ~90% (60% on verified bodies) |
| 2026-10-13 (last) | minor | Pre-X FinePix SuperCCD RAF decoded (`raf_superccd.rs`: 45° lattice in both storage layouts demosaiced and resampled upright, S2Pro big-endian and turned, SR Pro S frame, SR S photosites upright); 19 CC0 FinePix files pinned; re-measured: 295 verified, 1,002 unverified, 114 preview, 35 unsupported; Fujifilm 99% fully working, use-weighted 89% (60% on verified bodies) |
| 2026-10-13 (latest) | minor | Z 8 and Z 9 14-bit lossless NEFs verified (CC0, pinned; `corpus_nikon_z8_z9_lossless`): 280 verified, 993 unverified; use-weighted 89%, 59% on verified bodies; raw.pixls.us has no Z 6III file |
| 2026-10-13 (later) | minor | ARW version 1 (`arw1.rs`: DSLR-A100, A200…A390) and the Z 6II's table-less packed NEF (#769) decode; 31 CC0 files pinned (one per popular Nikon, Fujifilm, Canon CR2 and Pentax body, the ARW1 bodies and the A850); re-measured: 278 verified, 995 unverified, 138 preview, 35 unsupported; use-weighted 89% (57% on verified bodies); decoding 80%, RAW readiness ~58% |
| 2026-10-13 | minor | Minolta MRW decoded (`vendor/mrw.rs`: 16-bit words and 12-bit packing, WBG white balance in layout order): 7 CC0 files from 6 bodies pinned and checked against the camera JPEG; re-measured: 247 verified, 1,016 unverified, 147 preview; use-weighted unchanged at 87% |
| 2026-10-12 | minor | 22 more CR3 bodies verified (one CC0 raw.pixls.us file each, pinned, checked against the camera JPEG; all 110 CC0 CR3s decode); EOS R6 Mark III ColorData 66 white balance: 259 verified, 919 unverified; use-weighted 92% (59% on verified bodies); Canon 73% → 94%; decoding 83%, RAW readiness ~56% → ~60%; CR3 gain +6.2 → +0.3 |
| 2026-10-12 | minor | 117 Nikon and Fujifilm bodies verified (one CC0 raw.pixls.us file each, pinned, checked against the camera JPEG; `body_corpus.rs`); COOLPIX 5700 / 8400 / 8800 decode wrongly (now unsupported); #702 fixed, A100 preview-only: 354 verified, 824 unverified, 230 preview, 38 unsupported; use-weighted 86% (58% on verified bodies); decoding 77% unchanged |
| 2026-10-11 | minor | Nikon: every body without `CropArea` surveyed (#1053); 36 model / size / depth groups cropped to their measured camera frame (#976, #914), 6 pending; NEF row no longer claims every coding but HE decodes (compressed NEFs without a linearization table listed) |
| 2026-10-10 | minor | Compressed ORF: pointer to the two Olympus patents that describe the method, as a clean-room starting point |
| 2026-10-12 | minor | PEF without area tags: masked border rows and columns trimmed against the BlackPoint (issue #1271; KP, KF, K-3 Mark III, K-70, K-1, K-1 Mark II, K-S1, K-S2, 645Z); counts unchanged |
| 2026-10-13 | minor | Compressed Samsung SRW (32770 row blocks, 32772 Huffman, 32773 blocks) decoded: 7 CC0 files from 7 bodies pinned and checked against the camera JPEG; re-measured: 241 verified, 1,011 unverified, 158 preview; use-weighted unchanged at 87% (Samsung carries no weight bucket) |
| 2026-10-13 | minor | Compressed ORF decoded (`decode_compressed`); E-M1 and E-M10 Mark III verified on the corpus; re-measured with `coverage.py`: 237 verified, 1,006 unverified, 167 preview; OM / Olympus bucket 3% → 80% fully working; use-weighted 87%; decoding 78%, RAW readiness ~57% |
| 2026-10-13 | minor | Lens profiles: Nikon in-file distortion (NikonNEFInfo `0xc7d5`) for the Z 6II, Z 8 and Z f, validated on 83 frames from three lenses and the CC0 Z f sample (`docs/nikon-lens-corrections.md`); lens score unchanged |
| 2026-10-11 | minor | EOS R5 Mark II verified (CRX QP-map first literal and limited-length escape, odd-height Bayer planes; 4 CC0 files pinned, QP maps of 33,665 private files): 235 verified, 942 unverified; use-weighted 85% (42% on verified bodies); decoding 77%, RAW readiness unchanged at ~56% |
| 2026-10-10 (later) | major | Model-by-model measurement against Adobe's published list (1,446 models: 234 verified, 943 unverified, 233 preview, 36 unsupported); `docs/raw/` data and re-measure script; use-weighted share 84% (40% on verified bodies); variant matrix; colour and lens sections with scores; ranked next cameras; RAW readiness ~56% from written parts |
| 2026-10-10 | major | Created: formats, makers, demosaic, colour and lens checklists; target counts from the installed Lightroom 9.6 bundle listing |
| 2026-10-10 (later) | minor | Lens profiles: Sony ILCE-7RM6 in-file distortion next to the ILCE-7RM4A's (its lateral CA parsed, off by default); lens score unchanged (~10%: one more body) |
| 2026-10-14 | minor | 97 Canon CR2, Pentax and Ricoh models verified (one CC0 raw.pixls.us file each, pinned, checked against the camera JPEG; `canon_pentax_corpus.rs`): 334 verified, 844 unverified, 232 preview; 50% of photographers on a verified body; use-weighted 85% unchanged |
| 2026-10-14 | minor | Integration batch 36: CC0 per-model verification for 117 Nikon/Fujifilm, 97 Canon CR2/Pentax/Ricoh, 21 Sony/Leica and 23 Olympus/Samsung bodies, 22 more CR3 bodies and 30 more models; Epson ERF and Mamiya MEF decode; measured Nikon image areas (#1053, #1770): 515 verified, 675 unverified, 218 preview, 38 unsupported (re-measured with `coverage.py`) |
| 2026-10-15 | minor | Upstream (batch 40): by-maker and by-use tables re-measured with `coverage.py` after batches 36–39 (545 verified, 651 unverified, 212 preview, 38 unsupported; ~94% of photographers' cameras fully work, ~85% on a verified body); rows the per-model PRs had updated are current again |
