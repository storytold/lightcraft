## Import

- **More CR3 variants decode:** Canon CRX partial subbands, multi-tile version 0x200 C-RAW and adaptive low-frequency quantization now decode from sensor data instead of falling back to the embedded preview; ported from LibRaw's `crx.cpp` (LGPL 2.1) and checked on synthetic streams, since no public file uses them.
