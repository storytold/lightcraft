# Security Policy

LightCraft reads camera raw files, image formats, metadata and sidecars, imports other photo catalogs, downloads and runs machine-learning models, and exposes CLI, JSON control, and MCP automation. Treat files, paths, metadata, catalogs, model files, automation requests, and command parameters as potentially malicious.

## Reporting a vulnerability

Please give the repository maintainers a reasonable opportunity to investigate and coordinate a fix before publishing exploitable details, proof-of-concept code, or weaponized files.

This repository does not currently document a dedicated private security email or private reporting channel. Contact the repository maintainers through the repository owner/maintainer channels already listed in the project, provide only a minimal non-exploitable summary in public, and ask for an appropriate private transfer method before sharing sensitive details. Do not open a public issue containing a working exploit, secret, sensitive crash dump, or malicious attachment.

If no private channel can be established, withhold weaponized material and report the smallest safe description that allows maintainers to make contact.

## What to include

A useful report includes:

- affected LightCraft version, commit, or release artifact;
- operating system, architecture, build profile (desktop or web), and relevant feature flags;
- the affected component or path;
- security impact and required attacker access;
- exact reproduction steps;
- expected behavior and actual behavior;
- crash logs, backtraces, sanitizer output, or resource measurements when applicable;
- whether the problem reproduces from a clean checkout;
- any known workaround or containment.

Remove credentials, personal paths, private photos, and unrelated system information from logs.

## Malicious test files

Do not attach a potentially harmful or non-redistributable file to a public issue. Initially provide its format, size, cryptographic hash, observed effect, provenance category (for example, synthetic or fuzz-generated), and the command needed to reproduce the problem. Coordinate a private transfer method with maintainers before sending the sample.

Test files accepted into the repository must be minimized, free of personal or proprietary data, legally redistributable, and documented with the expected failure behavior. A fixed parser defect should receive a permanent regression test whenever safe and practical.

## Scope

Security reports may cover:

- camera raw, DNG, TIFF, HEIF, JPEG and other image decoding, and EXIF/XMP metadata and sidecar parsing;
- decompression bombs, oversized dimensions, integer overflow, memory exhaustion, recursion, or hangs;
- the LightCraft catalog, and importing other catalogs and libraries (such as Lightroom catalogs);
- import/export and filesystem path handling, including traversal, symlinks, and unsafe overwrite or move behavior;
- downloading, verifying, unpacking and running machine-learning models (denoise, faces, masks);
- engine command validation and serialization/deserialization;
- the desktop control channel, the MCP server, and the MCP-to-app connection;
- the WebAssembly build;
- GPU resource validation or driver-facing input handling;
- dependencies, CI, packaging, signing, checksums, and release artifact integrity.

General bugs without a security impact should use the project's normal issue and contribution process.

## Current limitations

The control channel (`lightcraft --control PORT`) listens on loopback only and has no authentication, so any local process can drive the app while it is enabled; only enable it when you need it ([control-protocol.md](docs/control-protocol.md)).
