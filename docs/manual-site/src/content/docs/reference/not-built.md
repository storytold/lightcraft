---
title: What is not built yet
description: Honest list of gaps, grouped.
sidebar:
  order: 5
---

- Chromaforge is pre-release. Every Classic module exists; the gaps are mostly quality and coverage, not missing buttons.
- Source: the repository's `ROADMAP.md` and `docs/gaps.md`.

## Raw and colour
- **Camera colour is estimated**, not measured for every body (fits to the camera JPEG, a few bundled profiles, spectral and dcraw matrices).
- **Some raws are preview-only**: unusual CR3 variants, Nikon High Efficiency on several bodies, NEF lossy-after-split, IIQ, X3F, ARQ pixel shift, old FinePix.
- **Per-model verification** is partial (about 41% of listed models).
- **Render fidelity** against Lightroom is tuned by eye, not measured.
- **Lens profiles**: no bundled database; in-file corrections and your own lensfun-format database only.
- **Per-monitor display profiles** on Windows and Wayland.

## AI
- **Masks**: Subject and Background need an optional download; Sky, People and Landscape are weaker than the target.
- **AI Denoise**: needs separately installed weights; no X-Trans model. Raw Details and Super Resolution are classical methods.
- **Generative remove** works through cloud providers with your own key; no on-device model. Remove people and reflections are not built.

## Library and output
- **Lightroom catalog import** is one-way and approximate; some rules and settings do not carry over.
- **Tethered capture** over USB is untested on real camera bodies; not on Android.
- **Slideshow video export** and sound on Linux/BSD are unfinished; print colour management is partial; no CMYK printer profiles (by decision).
- **Multi-photo metadata editing** does not show "mixed" values.
- **Folder-watch URL scheme** is not registered with the operating system.

## Platforms and quality
- **Android and device sync** are tested on an emulator and one machine only.
- **Accessibility**: screen-reader audit and keyboard-only masking pins are open.
- **Translations**: 17 languages; What's New is mostly English.
- **HDR** on X11 is a platform limit; video has no sound output yet.
- **Control surfaces**: Loupedeck Live / CT and Tangent panels are not built; MIDI is Windows and macOS only.
- **Command palette**: no palette yet; use Settings ▸ Shortcuts to search commands.
