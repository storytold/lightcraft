# Feature naming

LightCraft uses **Perspective Correction**, **Guided Perspective**, and **Perspective Guides** for its own geometry controls. The section includes Off, Auto, Guided, Level, Vertical and Full modes, so its name does not imply every mode is automatic.

The previous Upright terminology is a naming-review candidate, not a verified finding about trademark registration. These descriptive names are chosen to distinguish LightCraft's own implementation.

## Compatibility boundary

The `geometry.upright`, `geometry.guides` and `tool.guidedUpright` commands, saved tool keys, Rust enum variants and serialized settings remain stable. XMP `crs:PerspectiveUpright` and `UprightTransform_*` mappings and Lightroom import messages still describe the external format accurately. No rendering, import or export logic changes.

All 17 shipped non-English catalogs contain the new labels. The normal CI, perspective-mode tests and a visual panel/tool check remain required before this draft is marked ready.

## Regression coverage

The standard-library-only `display_names` integration tests scan production Rust string literals for the retired display names and preserve explicit compatibility exceptions. They also test identifier, comment and raw-string handling. These focused checks do not replace application or visual testing.

## Import backup copy

The import dialog's option to copy each imported file to a second folder is labelled **Backup to**, and its parameter is `backup`. Both are ordinary descriptive words for what the option does: it keeps a backup copy. The label does not use another photo library's name for its version of this option. Checked 2026-11-27 against the naming standard's names list; no trademark search was run, since the label is plain descriptive wording.
