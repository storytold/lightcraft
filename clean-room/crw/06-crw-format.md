# Canon CRW raw compression — derived format (FROZEN)

Status: **frozen at this commit** (plan §5 P4). From here on, the derivation is not
tuned; any later correction must reopen it explicitly with a recorded cause and a new
freeze commit. Everything here was derived from the CC0/private sample files
and black-box probing of LibRaw 0.22.2 (`unprocessed_raw.exe`), per the approved plan; no
decoder source was consulted. The chronological evidence is `04-derivation-log.md`; the
container and payload groundwork are `02-container.md` and `03-payload-layout.md`.

## 1 Payload layout

RawData (CIFF record 0x2005), with w×h from SensorInfo (0x1031) and
(selector, 0, entropyOffset = 514, compressedLength) from DecoderTable (0x1835):

```
[ low-bits plane, ceil(w*h/4) bytes ]   present on some bodies (rule below)
[ 514 zero bytes ]                      (the DecoderTable's entropyOffset)
[ entropy bitstream, ff00-stuffed ]
[ tail: 00 00 / ff 00 00 (EOS) or ff d9 (+00) (PowerShot) ]
```

- **Low-bits plane**: 2 bits per sensor sample, row-major, LSB-first within each byte
  (sample 4i in bits 1:0 … sample 4i+3 in bits 7:6). Present ⇔ the first
  w·h/4 + 514 bytes of the payload contain any 0xff byte NOT followed by 0x00
  (the entropy stream is always stuffed, the low-bits plane is raw — this scan is the
  discriminator; verified: present on D30/D60/10D/300D/G5/G6, absent on G1).
- On the EOS bodies the DecoderTable's compressedLength excludes the low-bits plane;
  on G5/G6 it includes it. The decoder should not rely on compressedLength: entropy
  data starts at (low-bits size, if present) + 514 and runs to the tail.

## 2 Entropy bitstream

After removing the stuffing (drop the 0x00 after every 0xff), bits are read
**MSB-first**. The stream codes the w×h sensor samples' 10-bit "high" plane
(the full sample is `(high << 2) | low_bits` where a low-bits plane exists, else
`high` itself):

- Samples are produced in row-major order, grouped in **blocks of 64 samples** laid
  flat over the frame (blocks cross row boundaries; w need not divide by 64).
- Two canonical Huffman code tables per DecoderTableNumber (0, 1, 2): the token that
  begins each block is read from the **first table**, every other token from the
  **second table**.
- A token decodes to a leaf **(r, s)** with r ≤ 15: skip r samples (each emits its
  DPCM chain value unchanged, i.e. a zero diff), then read s magnitude bits and emit
  one diff (s = 0 emits a zero diff). Magnitudes follow the JPEG rule, MSB-first:
  m ≥ 2^(s−1) → d = +m, else d = m − 2^s + 1. Sample diffs fit s ≤ 10 (10-bit high
  plane), but first-table leaves reach s = 11+: the carry-coded block DC difference
  spans ±2046.
- The **(0,0) leaf read as a non-first token is EOB**: the rest of the block emits
  zero diffs. ((0,0) as the block's first token is a plain zero diff.)
- **Carry**: the diff landing on each block's first sample has the previous block's
  (combined) first diff added: `d0 += carry; carry = d0`. When the first sample is
  covered by a token's skip (or EOB), its diff is exactly `carry` (and carry is
  unchanged).
- **DPCM chains**: two running predictors by sample parity, both initialised to 512,
  and both **reset to 512 at the start of every sensor row**.
- If the stream ends before the frame is complete, remaining samples emit their chain
  values (observed: the final 2–3 samples of each file).

## 3 Code tables

Extracted from the samples by known-plaintext alignment and frozen as data:

- Selector 0: `logs/table_sel0.json` — 12 first-table + 154 second-table codes.
- Selector 1: `logs/table_sel1.json` — 12 + 161.
- Selector 2: `logs/table_sel2.json` — 12 + 162.

Tables are shared across bodies per selector (verified across D60/10D/300D/Rebel for
selector 0; D30, G1 and the private D60s for selector 1; 10D, 300D and a private D60
for selector 2). The tables have canonical Huffman structure; codes up to 16 bits, the
`11111111xxxxxxxx` region carrying the larger (run, size) pairs. The JSON rows are a
superset of each file's exercised codes: every row was learned from a real stream
position and validated by the decode reaching the end of that frame; rows never
exercised by two files may in principle be mergeable duplicates of canonical slots,
which the Rust implementation may normalise, provided the verification table of
`07-verification.md` still reproduces exactly.

## 4 Not yet established / out of scope here

- **G5 anomaly**: its entropy stream decodes end-to-end with the selector-1 tables,
  but values diverge beginning at sample 2056 (= 4 × 514), and its low-bits plane fits
  no tested layout (top rows mostly match, decaying to 0 %, no constant shift). The
  decoder must detect the G5 (CanonModelID / make-model) and fall back to the embedded
  JPEG until a follow-up derivation resolves it.
- The exact meaning of each file's tail bytes (handled by the stream-end rule).
- Big-endian (`MM`) CIFF files: no sample exists.
- PowerShot G2/G3/Pro1/Pro70/S30–S70: no CC0 samples (licence-excluded); decodable
  speculatively via the same selectors once their geometry is read from SensorInfo,
  to be marked unverified.
- Black level, white balance and colour (plan P3) are documented separately.

## 5 Verification

See `07-verification.md` (written after the freeze): full-frame exact-match decode
against LibRaw 0.22.2 for every sample file, public and private, with zero tuning
after the freeze commit.
