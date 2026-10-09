# ORF research provenance and acceptance gates

Task: `LR-IMP-FORMATS` / M11.3. Started 2026-10-08. Baseline:
`629e39380e296f588c64cd9c0053a8edc3528f36` of storytold/lightcraft.

## Inputs and exposure disclosure

- Read: LightCraft's existing MIT/Apache code, the user-provided photographs,
  published metadata descriptions, PR #240's description and maintainer discussion.
- Not used as inputs: PR #240's `orfc.rs`, its compressed encoder, other ORF
  decoder source, camera coefficient tables, Adobe profiles or lens profiles.
- The author is Codex. No assertion is made that an AI model's training contains
  no decoder knowledge. Instead, every new packing rule is tied to measurements
  collected before reference comparison. No compressed rule is supplied from
  remembered code or reconstructed from the rejected contribution.
- Metadata and simple packed-pair fixtures are independently generated with
  LightCraft's existing TIFF writer. They do not encode a compressed ORF stream.
- The creator has explicitly released all ten initial originals as CC0 in a
  separate [public corpus repository](https://github.com/voshart/Rust-Olympus-RAW-decoder/tree/4d29fe4886d893883a78f6099f02d8dd17d47192/corpus/voshart-olympus).
  They remain unchanged. No media, derived sensor arrays or binary instruments
  are committed to LightCraft. A public E-M5 II held-out sample now also passes
  every-sample comparison; broader camera/mode claims still need distinct evidence.
- Additional inputs: fourteen licence-verified external CC0 specimens and their
  primary corpus metadata. No third-party decoder source or corpus script was
  opened. See [source checks and findings](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/4d29fe4886d893883a78f6099f02d8dd17d47192/research/public-samples.md).
- The creator subsequently contributed two more E-M5 III ORFs under CC0. The
  corpus now has ten unchanged ORFs and two JPEGs. The nine compressed creator
  originals and eight external controls have complete numerical comparisons in
  [research commit 9d8c927](https://github.com/voshart/Rust-Olympus-RAW-decoder/tree/9d8c927b8f181c6eea4358db30b3a71641922aaf).
- The token/state/predictor hypothesis was captured in a 96-file local immutable
  snapshot before two user-supplied source-derived prose explanations were read.
  The first-row work was already published; later-row work was locally uncommitted.
  A generic libopenraw container-recognition snippet appeared incidentally in a
  documentation search; its source page was not opened, and it supplied no
  compressed coding rule. The later full-frame checks use the unchanged frozen
  model. Generalized 14-bit/header interpretations from the supplied prose are
  external claims, not independently established rules. Full exposure details,
  document hashes, checkpoint identities and failed hypotheses are in the pinned
  [research provenance](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/orf12-provenance.md).

## Evidence trail

| Id | Experiment | Result | Limit |
|---|---|---|---|
| C1 | `inspect_orf.py`, eight ORFs and two JPEGs, hashed before use | Models, lenses, dimensions, CFA and strip budgets recorded | Local specimen coverage only |
| C2 | Compare E-M5 II Exif CFA in ordinary/high-resolution modes | RGGB versus GRBG | No model-table inference |
| P1 | Divide high-resolution strip count by height and width | 14,848 bytes/row; ten samples/16 bytes | One initial original, later released as CC0 |
| P2 | Count padding and compare low/high-nibble hypotheses in masked columns | 6,432,896 zero pads; sharply different border distributions | Statistics alone do not prove packing |
| P3 | Compare the Python hypothesis and final Rust reader to black-box sensor output | All three full-sensor hashes agree; 64,328,960 identical samples, including borders | Reference implementation may itself have bugs |
| P4 | Public held-out PIXLS.US 2856 | Python, final Rust reader and reference match all 64,328,960 sensor samples | Same E-M5 II mode; no broader camera claim |
| S1 | Independently generated packed and Exif fixtures | Four Bayer layouts/two byte orders, boundary values, truncations and mutations | Round trips are supporting evidence, not independent correctness |
| S2 | Synthetic maker-note crop at u64::MAX | Old unchecked crop addition overflows; checked coordinates fall back to the sensor area | Non-conforming numeric tag type |
| X1 | Isolated compressed-strip byte perturbations | Reproducible reference failures/difference extents | Does not establish compressed coding rules |
| X2 | Independent first-row, reset and predictor families, then complete E01/V02 candidate rasters saved before full native comparison | Complete sensor equality across the examined compressed originals, with no margins excluded | Research stage; one binary reference family |
| X3 | Final-byte influence, procedural reader oracle and final-token byte cuts | Twenty-four mutations include fourteen unused-bit no-effect cases; thirty required-byte cuts reject | No universal padding rule or completed product fuzzing suite |
| X4 | Separate specification/arithmetic review, then safe Rust implementation and full-sensor replay | All examined product output hashes match the recorded complete reference rasters; header/full metadata agree | Same AI-assisted author researched, reviewed and implemented; not independent third-party approval |
| X5 | Product literal vectors, hostile-input checks, full CI and application imports | Twenty-one focused tests; all seven CI gates pass; native/headless imports verified as editable raw | Bounded test coverage, not exhaustive fuzzing, colour parity or all Olympus variants |

Local artifacts: `plan/orf-research/private-manifest.json`,
`packed12-reference.json`, `compressed-perturbations.json`, and the Rust audit
sensor dump. Input and output hashes make accidental specimen changes detectable.
Tools write observations with create-new semantics and open photographs read-only.
The separate public repository versions the released originals, whitelisted
observations, instrument versions and per-file comparison hashes; derived sensor
arrays and reference binaries remain local.

## External measuring instrument

The separately installed binary Python package rawpy **0.27.1** uses LibRaw
**0.22.1**. NumPy **2.4.3** supports array comparisons. They are installed only
in the gitignored local research directory; none is a Cargo dependency, shipped
asset or runtime dependency. Their source was not opened. Only sensor arrays and
geometry were read; colour matrices and camera tables were not consulted.

Pinned Windows CPython 3.13 wheel hashes (SHA-256):

| Wheel | Hash |
|---|---|
| rawpy-0.27.1-cp313-cp313-win_amd64.whl | 1d2d32bfe7df6421f214f0502d34bd599ce53156401928c4f538336f2f8b3689 |
| numpy-2.4.3-cp313-cp313-win_amd64.whl | 0a60e17a14d640f49146cb38e3f105f571318db7826d9b6fef7e4dce758faecd |

Binary-distribution sources: [rawpy on PyPI](https://pypi.org/project/rawpy/0.27.1/),
[NumPy on PyPI](https://pypi.org/project/numpy/2.4.3/).
The optional instrument is not needed to build, test or run LightCraft.

## Before a compressed decoder is acceptable

1. Independently derive the bitstream description. For each non-obvious rule,
   record specimen hash, operation, competing hypotheses and discriminating
   observations. Preserve failed hypotheses rather than retrofitting a story.
2. Review the specification separately from code. Disclose exposure to other
   implementations; a new AI session alone is not provenance evidence.
3. Implement only the approved specification, with independently supported test
   vectors. Keep unknown variants explicitly unsupported.
4. Compare every sensor sample on held-out files, stating the full dimensions,
   active crop, CFA origin and any unverified margins. Add bounded allocation,
   arithmetic, truncation and mutation coverage. Run the full project CI.
5. Keep camera colour and lens correction acceptance separate from unpacking.
   Accurate samples do not establish Lightroom rendering parity.

This work satisfies the observed padded packed-layout gate on two E-M5 II files
and numerical full-raster checks of the measured compressed 12-bit profile on
the examined originals. The separate [specification/arithmetic review](orf12-review.md)
preceded product implementation. The [published Rust validation](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/acecb73b4107a95ea256017a8f389f74a8c98d29/research/rust-implementation.md)
records source/input/output hashes, safety coverage and CI results. Review and
implementation were performed by the same AI-assisted author, not an independent
third party; upstream maintainers still decide whether the provenance meets their
policy. Generalized 14-bit coverage, other compressed fingerprints and colour
calibration remain outside this contribution. A rights-holder grant is an
alternative to independent derivation only for exactly the implementation covered
by that grant.
