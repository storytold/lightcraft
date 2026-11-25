# Nikon embedded distortion corrections

LightCraft uses the distortion warp that craft-raw reads from supported full-frame Nikon Z 6, Z 7, Z 6II, Z 8 and
Z f NEFs. Modes 1 (On Optional) and 3 (On Required) supply a warp. Modes 0, 2 (Off) and unknown values do not.
The shared decoder documents the file layout, camera support and validation against camera JPEGs.
[Decoder and validation notes](https://github.com/storytold/craft-libs/blob/23acc704933a9a5619a5cd1cddc9bb1fb5a9b2d3/crates/raw/docs/nikon-lens-corrections.md)

LightCraft enables the embedded profile on import and gives it precedence over a lens-database profile. The profile
switch and distortion amount control the warp in previews and rendered exports. The correction follows the photo's
orientation. DNG export preserves the warp with the sensor data, including when the profile is switched off for
the current edit. Nikon vignetting remains unsupported.

## Existing libraries

Reload from Disk refreshes the catalog's lens metadata. It enables a newly discovered profile on an untouched photo
without an import preset. A photo with saved edits keeps its settings, including an explicit off switch.

The camera-look fit uses the recognized geometry even when the profile switch is off. Decoding an existing edit
again can therefore change its tone or colour on any process version. LightCraft advances the camera-look and
render-cache versions so it can refresh these fits and thumbnails. An offline original keeps its old smart preview.
After reconnect, Build Smart Previews reads the original even if the loupe has cached the offline proxy.
[Rendering process versions](process-versions.md)

## Render evidence

The 2026-11-05 CPU export comparison covered 40 pinned inputs. All six Z 6 and Z 7 exports matched their camera
JPEGs more closely in luminance correlation. The other 34 outputs had identical RGB pixels. The six exposures cover
two scenes with one NIKKOR Z 24-70mm f/4 S at 52 and 56 mm. The extreme corners remain unvalidated. The comparison
records the original binaries, input hashes and per-file results. [Render comparison](raw/nikon-distortion-sweep.tsv)
