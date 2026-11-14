# Functional Specification: Olympus ORF Compressed Bitstream & Predictive Decompression

**Document:** `clean-room/orf/orf-spec.md`  
**Role:** Specification Author (Clean-Room Boundary)  
**Applicability:** Olympus / OM System compressed raw images (`.ORF`, `.ORI`)  
**Derived From:**
1. US Patent US7333035B2 (Takashi Ishikawa / Olympus Corp., *"Image data compression apparatus, image data decompression apparatus, image data compression method, and image data decompression method"*).
2. US Patent US7715639B2 (Takashi Ishikawa / Olympus Corp., *"Image data compression apparatus, image data compression method, and computer-readable recording medium"*).
3. TIFF Revision 6.0 Specification (Aldus / Adobe).
4. Exif 2.3 Specification (CITA / JEITA) & Olympus MakerNote directory layout.
5. Black-box observation of CC0 sample files from `raw.pixls.us`.

---

## 1. Container & Bitstream Framing

Compressed Olympus ORF and ORI files adhere to the TIFF 6.0 container specification with proprietary Olympus identification headers:
- **Magic Number:** Big-endian `0x4949524F` (`"IIRO"`, little-endian byte ordering) or `0x4D4D4F52` (`"MMOR"`, big-endian byte ordering).
- **IFD0 Attributes:**
  - `ImageWidth` (Tag `0x0100`): Full sensor matrix width $W$ in photosites.
  - `ImageLength` (Tag `0x0101`): Full sensor matrix height $H$ in photosites.
  - `BitsPerSample` (Tag `0x0102`): Typically declared as 16 (or 12).
  - `Compression` (Tag `0x0103`): Value 1 (uncompressed TIFF container wrapping an opaque proprietary payload).
  - `StripOffsets` (Tag `0x0111`) & `StripByteCounts` (Tag `0x0117`): Location and length of the encoded sensor strip.

### Stream Prefix
Within the payload strip:
1. The stream begins with an $N$-byte big-endian unsigned integer prefix (where $1 \le N \le 8$, typically 4 or 8, is specified by ImageProcessing tag `0x0653`). Valid compressed streams require this value to equal `1`.
2. The $N$-byte prefix is followed immediately by a two-byte sentinel delimiter: `[0x00, 0x00]`.
3. The compressed entropy bitstream begins immediately at byte index $N + 2$.

---

## 2. Decompression Parameters (`ImageProcessing` SubIFD `0x2040`)

The Olympus MakerNote (Exif tag `0x927c`) contains the `ImageProcessing` SubIFD at tag ID `0x2040`. The decompressor requires the following parameter set:

| Parameter Symbol | SubIFD Tag | Description | Typical Value | Allowed Range |
| :--- | :--- | :--- | :---: | :---: |
| $S_0$ | `0x0640` | Direct uncompressed low-order bit depth per photosite | 0 | $0 \le S_0 \le 16$ |
| $T_E$ | `0x0641` | Directional gradient threshold exponent ($E_{th} = 2^{T_E}$) | 5 | $0 \le T_E \le 16$ |
| $P_0$ | `0x0642` | Default boundary baseline predictor value | 0 | $-65536 \le P_0 \le 65536$ |
| $S_R$ | `0x0643` | Direct residual sub-bit depth | 2 | $0 \le S_R \le 2$ |
| $D_{fb}$ | `0x0644` | Context feedback divisor bit-shift | 5 | $1 \le D_{fb} \le 15$ |
| $T_{flat}$ | `0x0645` | Flatness threshold exponent ($F_{th} = 2^{T_{flat}}$) | 4 | $0 \le T_{flat} \le 16$ |
| $R_{th}$ | `0x0646` | Flatness run-length threshold | 3 | $0 \le R_{th} \le 32$ |
| $A_0$ | `0x0647` | Adaptive Rice parameter base A (flat region) | 2 | $0 \le A_0 \le 15$ |
| $A_{lim}$ | `0x0648` | Adaptive Rice parameter limit A (flat region) | 3 | $0 \le A_{lim} \le 15$ |
| $B_0$ | `0x0649` | Adaptive Rice parameter base B (active region) | 4 | $0 \le B_0 \le 15$ |
| $B_{lim}$ | `0x0650` | Adaptive Rice parameter limit B (active region) | 7 | $0 \le B_{lim} \le 15$ |
| $k_{max}$ | `0x0651` | Maximum allowable Golomb-Rice parameter $k$ | 15 | $0 \le k_{max} \le 15$ |
| $Q_{max}$ | `0x0652` | Maximum unary quotient run before escape coding | 12 | $1 \le Q_{max} \le 32$ |
| $N$ | `0x0653` | Header prefix length in bytes | 4 or 8 | $1 \le N \le 8$ |
| $B_{sensor}$ | `0x102c` | MakerNote valid sensor bit depth | 12 | $8 \le B_{sensor} \le 16$ |

---

## 3. Bitstream Model

The bitstream is packed MSB-first across big-endian byte sequences:
- When fetching $m$ bits, the most significant remaining bit in the stream corresponds to the highest bit of the returned word.
- An internal FIFO bit reservoir (e.g. 32-bit or 64-bit word accumulator) buffers incoming bits and reloads from stream memory as bits are consumed.
- Stream exhaustion before the full frame $W \times H$ is parsed indicates a truncated or corrupt file.

---

## 4. Entropy Decoding Algorithm (US7333035B2)

Photosites are processed sequentially in row-major order: row $y \in [0, H-1]$, column $x \in [0, W-1]$.

### Step 4.1: Bitfield Extraction
For each photosite:
1. **Sign Flag:** Read 1 bit from the stream: $s \in \{0, 1\}$.
2. **Residual Sub-Bits:** If $S_R > 0$, read $S_R$ bits as unsigned integer $u_{sub}$; otherwise $u_{sub} = 0$.
3. **Low-Order Direct Bits:** If $S_0 > 0$, read $S_0$ bits as unsigned integer $u_{low}$; otherwise $u_{low} = 0$.

### Step 4.2: Quotient ($Q$) Unary Decoding & Escape Handling
Using the current Golomb-Rice parameter $k \in [0, k_{max}]$:
1. Count consecutive zero bits in the bitstream until a `1` bit is encountered:
   - Initialize count $z = 0$.
   - While reading next bit $b$:
     - If $b == 1$, break.
     - Increment $z \leftarrow z + 1$.
     - If $z == Q_{max}$:
       - **Escape Mode:** Unary run limit reached.
       - Set $Q = 0$.
       - If $k < 15$:
         - Read $15 - k$ bits from the stream as $Q$.
       - Read and discard 1 boundary bit.
       - Break from unary loop.
   - If escape mode was not triggered, set $Q = z$.

### Step 4.3: Remainder ($R$) Extraction & Magnitude
1. If $k > 0$, read $k$ bits from the bitstream as unsigned integer $R$; otherwise $R = 0$.
2. Form the unsigned symbol magnitude:
   $$M = (Q \ll k) \mid R$$
3. Convert magnitude to signed residual $\Delta$:
   - If $s == 1$: $\Delta = -(M \text{ as } i32) - 1$ (equivalent to two's complement inversion: $(M \oplus \text{0xFFFF\_FFFF}) \text{ as } i32$).
   - If $s == 0$: $\Delta = M \text{ as } i32$.

---

## 5. Context Adaptation & Rice Parameter $k$

Because the Bayer CFA arranges identical color filters at 2-photosite horizontal strides, context adaptation maintains a two-position magnitude history $(M_{pipe}[0], M_{pipe}[1])$ corresponding to $(x - 2, x - 1)$:
- At the start of each row, local context tracking state is reset:
  - Magnitude delay pipeline registers: $M_{pipe}[0] = 0, M_{pipe}[1] = 0$.
  - Flatness history delay registers: $C_{flat}[0..3] = 0$.

For each column $x$ in row $y$, prior to entropy decoding the photosite's symbol:
1. Retrieve preceding same-Bayer-channel magnitude:
   $$p = M_{pipe}[0]$$
2. Shift the two-position magnitude pipeline:
   $$M_{pipe}[0] \leftarrow M_{pipe}[1]$$
3. If $x > 1$:
   - Check flatness condition: if $p \le 2^{T_{flat}}$, increment flat count:
     $$sel = C_{flat}[3] + 1$$
   - Else:
     $$sel = 0$$
   - Shift context pipeline:
     $$C_{flat}[3] \leftarrow C_{flat}[2]$$
     $$C_{flat}[2] \leftarrow sel$$
   - Determine highest bit position $h$ of the 16-bit word $(p \ \& \ \text{0xFFFF})$:
     $$h = \begin{cases} 0 & \text{if } p == 0 \\ 16 - \text{leading\_zeros}(p \ \& \ \text{0xFFFF}) & \text{if } p > 0 \end{cases}$$
   - Select parameter table:
     $$\text{If } sel \ge R_{th}: \quad t_{lim} = A_{lim}, \quad t_{base} = A_0$$
     $$\text{Otherwise}: \quad t_{lim} = B_{lim}, \quad t_{base} = B_0$$
   - Compute adapted Rice parameter:
     $$k_{cand} = t_{base} + \max(-1, h - t_{lim}) + 1$$
   - Clamp $k = \min(k_{cand}, k_{max})$.
4. If $x \le 1$:
   - Set $k = B_0$.
5. After calculating $k$, entropy-decode the current symbol per Section 4 to obtain magnitude $M$ and signed residual $\Delta$.
6. Store current magnitude into the delay pipeline for subsequent steps:
   $$M_{pipe}[1] \leftarrow M$$

---

## 6. Spatial Directional Prediction (US7333035B2)

Because the image is captured via a Bayer color filter array, same-color photosites occur at intervals of 2 pixels horizontally and 2 pixels vertically.

For photosite $(x, y)$:
- **West Neighbor ($W$):** Reconstructed value of $(x - 2, y) \gg S_0$, or $P_0$ if $x < 2$.
- **North Neighbor ($N$):** Reconstructed value of $(x, y - 2) \gg S_0$, or $P_0$ if $y < 2$.
- **North-West Neighbor ($NW$):** Reconstructed value of $(x - 2, y - 2) \gg S_0$, or $P_0$ if $x < 2$ or $y < 2$.

### Edge-Directed Predictor Formulation
Calculate the spatial gradient differences:
$$\Delta_H = |W - NW|$$
$$\Delta_V = |N - NW|$$
Gradient threshold: $E_{th} = 2^{T_E}$.

The directional predicted baseline $\hat{P}$ is selected as:
1. If $W < N$ or $NW < W$:
   - If $NW \le N$ and $W \ge N$: $\hat{P} = W$
   - Else if $NW < N$ or $W \ge N$:
     - If $NW > W$ or $W \ge N$:
       - If $\Delta_V > E_{th}$: $\hat{P} = N + W - NW$
       - Else if $\Delta_H \le E_{th}$: $\hat{P} = (N + W) \gg 1$
       - Otherwise: $\hat{P} = N + W - NW$
     - Else: $\hat{P} = W$
   - Else: $\hat{P} = W$
2. Otherwise: $\hat{P} = N$

*(Note to implementer: Ensure all intermediate calculations use saturating or checked 32-bit signed arithmetic to prevent arithmetic overflow on corrupted input data).*

---

## 7. Residual Sub-Bit Adjustment & Pixel Reconstruction (US7715639B2)

### Step 7.1: Sub-Bit Adjustment
If $S_R > 0$:
- To match identical Bayer color positions, feedback predictor values are delayed by two photosites using a two-stage shift register $(v_{pipe}[0], v_{pipe}[1])$:
  - At the start of each row: $v_{pipe}[0] = 0, v_{pipe}[1] = 0$.
  - Let feedback predictor $v = 0$ if $x < 2$, else $v = v_{pipe}[0]$ (retaining feedback from two photosites earlier).
- Let sum $s = v + \Delta$.
- If $S_R == 1$:
  $$\Delta_{adj} = u_{sub} + 2 \cdot s$$
  $$v_{res} = v + (\Delta_{adj} \gg 1)$$
- If $S_R == 2$:
  $$\Delta_{adj} = u_{sub} + 4 \cdot s$$
  $$v_{res} = ((\Delta_{adj} \gg 1) \ \& \ \sim 1) + v + (\Delta_{adj} \gg 2)$$
- Shift and update feedback pipeline:
  $$v_{pipe}[0] \leftarrow v_{pipe}[1]$$
  $$v_{pipe}[1] \leftarrow v_{res} \gg D_{fb}$$
If $S_R == 0$:
- $\Delta_{adj} = \Delta$.

### Step 7.2: Final Sample Reconstruction
1. Baseline combination:
   $$P_{recon} = \Delta_{adj} + \hat{P}$$
2. Re-insert low-order direct bits:
   $$\text{Sample} = (P_{recon} \ll S_0) + u_{low}$$
3. Clamp $\text{Sample}$ to 16-bit unsigned integer range: $[0, 65535]$.
4. Store in the reconstructed output matrix at $(x, y)$.

---

## 8. Verification Criteria

A conforming clean-room implementation must satisfy:
1. **Safety:** Non-test code must forbid unsafe code (`#![forbid(unsafe_code)]`) and have zero panics (`clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`). Truncated or hostile bitstreams must return `RawError::Corrupt`.
2. **Image Fidelity:** Decoded mosaic green photosites across active areas must correlate strongly ($r > 0.60$) with camera-embedded JPEG previews on all CC0 corpus files (e.g., OM-D E-M1, OM-D E-M10 Mark III).
3. **Provenance Integrity:** The implementation must be original Rust code structured around clean-room principles, with no architectural mirroring or identifier retention from GPL/LGPL sources.

