# Preview size

Implementation in progress for [#1897](https://github.com/storytold/lightcraft/issues/1897).

Settings → Performance will offer a percentage of the photo's long edge (0–100%) and a
pixel limit (`-1` for no user pixel limit). The smaller limit wins. Zero percent must
still produce a usable minimum preview, never a zero-sized image. Existing saved
Automatic and fixed-pixel preferences must retain their current behavior on upgrade.

These controls set preview quality for interactive browsing and editing. Export sizes
and original files remain independent. Image bounds, GPU texture limits, and bounded
allocation still apply when the user removes the pixel limit. The interface will explain
how the controls affect previews and zoomed detail.

Before this contribution is ready: regression coverage for percentage and pixel limits,
saved preferences and invalid input; fit and zoom checks; native app screenshots;
translations in every shipped catalog; and `cargo xtask ci` on the submitted source.
