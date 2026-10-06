# lightcraft-sysraw

A small, safe Rust boundary for the installed macOS Core Image RAW service. It returns calibrated extended-linear Rec.2020 floats and fixed linear/display proxies, never copied camera profiles or JPEG replacement pixels. Product callers remain safe Rust; Objective-C/CF calls and ownership are confined to this crate.

`decode(bytes, max_edge)` bounds input, output dimensions and allocations, serializes decodes, validates finite output, and returns errors for unavailable APIs or unsupported images. Sony ARW is the current format hint. macOS 12+ is required; other platforms return an unavailable error and the engine uses its portable path. Float working precision is explicit; native sharpening, noise reduction, local tone and lens corrections are disabled so LightCraft controls remain editable. Capture WB and system baseline exposure are retained. Core Image handles orientation once.

The opt-in `real_arw_retains_linear_headroom_and_stable_proxies` test takes a local private fixture via `LIGHTCRAFT_TEST_ARW`. It is ignored in ordinary CI and asserts highlight headroom and resolution-independent proxies. No fixture/profile is committed. See `docs/camera-preview-colour.md` for tone validation, fallback and scope.
