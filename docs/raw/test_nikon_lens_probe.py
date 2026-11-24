"""Synthetic checks for the Nikon measurement tool, with no media fixtures.

Run: python3 -m unittest discover -s docs/raw -p 'test_nikon_lens_probe.py'
"""

import struct
import unittest
from pathlib import Path
from unittest import mock

import numpy as np
from PIL import Image

import nikon_lens_probe as probe


class Measurements(unittest.TestCase):
    def test_lab_score_sign_and_outer_ring(self):
        """An identical ramp has zero error; a darker render has negative dL."""
        ramp = np.tile(np.arange(32, 232, dtype=np.uint8), (133, 1))
        reference = Image.fromarray(np.repeat(ramp[..., None], 3, axis=-1))
        score = probe.score(reference, reference)
        self.assertAlmostEqual(score["r"], 1)
        self.assertEqual(score["mean_dl"], 0)
        self.assertEqual(score["outer_mae_dl"], 0)
        self.assertEqual(score["outer_pixels"], 5040)
        darker = Image.fromarray(np.repeat((ramp - 10)[..., None], 3, axis=-1))
        self.assertLess(probe.score(darker, reference)["mean_dl"], 0)
        with self.assertRaisesRegex(ValueError, "constant image"):
            probe.score(Image.new("RGB", (200, 133)), reference)

    def test_ifd_ranges_are_bounded(self):
        """A truncated directory or out-of-file entry fails before unpacking it."""
        for data, offset in [(b"", 0), (b"\0\0", -1), (b"\1\0", 0)]:
            with self.assertRaises(ValueError):
                probe.tiff_entries(data, offset, "<")
        directory = struct.pack("<HHHII", 1, 6, 7, 100, 500)
        with self.assertRaisesRegex(ValueError, "entry range"):
            probe.tiff_entries(directory, 0, "<")

    def test_geometry_requires_radial_matches_and_valid_fit_denominators(self):
        raw = dict(crop=dict(width=1920, height=1280))
        meta = dict(center=[0, 0], distortion_k=[0, 0, 0], zoom=0)
        # Twenty-one reliable matches can all lie in the excluded central disk.
        points = np.tile([239.5, 159.5, 239.5, 159.5, 239.5, 159.5], (21, 1))
        with self.assertRaisesRegex(ValueError, "radial.*matches"):
            probe.geometry_details(points, raw, meta)
        points[:, 0] += 100
        points[:, 2] += 100
        points[:, 4] += 100
        with self.subTest(zoom=-1), self.assertRaisesRegex(ValueError, "zoom.*denominator"):
            probe.geometry_details(points, raw, dict(meta, zoom=-1))
        no_measured_radius = points.copy()
        no_measured_radius[:, 4:6] = [239.5, 159.5]
        with self.assertRaisesRegex(ValueError, "zoom fit.*denominator"):
            probe.geometry_details(no_measured_radius, raw, meta)
        rho2 = (400 / (np.hypot(1919, 1279) / 2)) ** 2
        with self.assertRaisesRegex(ValueError, "zoom fit.*denominator"):
            probe.geometry_details(points, raw, dict(meta, distortion_k=[0, 0, -1 / rho2]))
        for invalid in (float("nan"), float("inf")):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "non-finite"):
                probe.geometry_details(points, raw, dict(meta, zoom=invalid))

    def test_truncated_nef_blocks_fail_before_indexing(self):
        """A version-only block must raise a useful error, not IndexError."""
        header = b"II*\0" + struct.pack("<I", 8)
        blob = b"Nikon\0" + bytes(4) + header
        for tag, name in (
            (5, "DistortionInfo"),
            (6, "VignetteInfo"),
            (8, "optical center"),
        ):
            entries = {5: bytes(72), 6: bytes(104), 8: bytes(20)}
            entries[tag] = b"0100"
            with self.subTest(tag=tag), mock.patch.object(
                Path, "read_bytes", return_value=header
            ), mock.patch.object(
                probe,
                "tiff_entries",
                side_effect=[{330: struct.pack("<I", 8)}, {0xC7D5: blob}, entries],
            ):
                with self.assertRaisesRegex(ValueError, "truncated " + name):
                    probe.nef_blocks(Path("unused.nef"))

    def test_one_tile_parity_is_not_a_brightness_validation_set(self):
        """Thousands of pairs from only one split cannot produce valid statistics."""
        raw = dict(crop=dict(x=0, y=0, width=1920, height=1280))
        meta = dict(
            center=[0, 0],
            zoom=0,
            distortion_k=[0, 0, 0],
            vignette_polynomial=[0, 0, 0, 0, 0, 0, 0.4, 0, 1],
            vignette_tail=0.4,
        )
        yy, xx = np.mgrid[:320, :480]
        parity = (xx // 24 + yy // 24) % 2
        for keep, missing in ((0, "validation"), (1, "training")):
            sensor = np.full((320, 480, 3), 0.3)
            sensor[parity != keep] = 0
            with self.subTest(keep=keep), mock.patch.object(
                probe, "sample", return_value=sensor
            ):
                with self.assertRaisesRegex(ValueError, "not enough " + missing):
                    probe.vignette_fit(
                        sensor, Image.new("RGB", (480, 320), (128, 128, 128)), raw, meta
                    )

    def test_radial_coverage_and_json_must_be_finite(self):
        """Neither missing corners nor non-finite values may be reported as evidence."""
        train = np.arange(1200) % 2 == 0
        with self.assertRaisesRegex(ValueError, "radial bin"):
            probe.validate_fit_samples(train, np.full(1200, 0.2))
        for value in (float("nan"), float("inf"), float("-inf")):
            with self.subTest(value=value), self.assertRaises(ValueError):
                probe.json_text({"statistic": value})

    def test_nonfinite_solver_statistics_are_rejected(self):
        """A failed numerical result must not reach the report serializer."""
        sensor = np.full((320, 480, 3), 0.3)
        raw = dict(crop=dict(x=0, y=0, width=1920, height=1280))
        meta = dict(
            center=[0, 0],
            zoom=0,
            distortion_k=[0, 0, 0],
            vignette_polynomial=[0, 0, 0, 0, 0, 0, 0.4, 0, 1],
            vignette_tail=0.4,
        )
        with mock.patch.object(probe, "sample", return_value=sensor), mock.patch.object(
            np.linalg, "lstsq", return_value=(np.full(22, np.nan), None, None, None)
        ):
            with self.assertRaisesRegex(
                ValueError, "non-finite brightness fit statistics"
            ):
                probe.vignette_fit(
                    sensor, Image.new("RGB", (480, 320), (128, 128, 128)), raw, meta
                )

    def test_opcode_gain_preserves_radius_center_and_eighth_power(self):
        """DNG normalization must retain a shifted center and the unnamed r^8 term."""
        raw = dict(
            crop=dict(x=8, y=8, width=6048, height=4024),
            active_area=dict(width=6064, height=4040),
        )
        meta = dict(
            center=[-90, 18],
            zoom=0.06,
            distortion_k=[0.02, -0.01, 0.04],
            vignette_polynomial=[-0.3, 0, 0.7, 0, -0.1, 0, 0.4, 0, 1],
        )
        ops = probe.opcodes(raw, meta, True)
        vignette = ops[0]["FixVignetteRadial"]
        warp = ops[1]["WarpRectilinear"]
        cx, cy = vignette["center"][0] * 6063, vignette["center"][1] * 4039
        self.assertAlmostEqual(cx, 8 + 6047 / 2 - 90)
        self.assertAlmostEqual(cy, 8 + 4023 / 2 + 18)
        farthest = np.hypot(max(cx, 6063 - cx), max(cy, 4039 - cy))
        half = np.hypot(6047, 4023) / 2
        for rho in (0, 0.25, 0.5, 0.75, 1):
            r2 = (rho * half / farthest) ** 2
            gain = 1 + sum(k * r2 ** (i + 1) for i, k in enumerate(vignette["k"]))
            self.assertAlmostEqual(gain, np.polyval(meta["vignette_polynomial"], rho))
            scale = sum(k * r2**i for i, k in enumerate(warp["planes"][0][:4]))
            self.assertAlmostEqual(
                scale, (1 + 0.04 * rho**2 - 0.01 * rho**4 + 0.02 * rho**6) / 1.06
            )

    def test_gain_fit_recovers_known_correction_with_unknown_tone(self):
        """A nuisance tone curve must not invent a different radial gain on a synthetic scene."""
        raw = dict(crop=dict(x=0, y=0, width=1920, height=1280))
        meta = dict(
            center=[0, 0],
            zoom=0,
            distortion_k=[0, 0, 0],
            vignette_polynomial=[-0.03, 0, 0.1, 0, 0.25, 0, 0.4, 0, 1],
            vignette_tail=0.4,
        )
        y, x = np.mgrid[:320, :480]
        rng = np.random.default_rng(20261105)
        texture = probe.ndimage.gaussian_filter(rng.uniform(-1, 1, (320, 480)), 2)
        light = 0.3 + 0.05 * np.sin(x * 0.09) + 0.04 * np.cos(y * 0.07) + texture * 0.15
        neutral = np.stack([light * 0.85, light, light * 1.1], axis=-1)
        _, _, r2 = probe.coordinates(raw, meta, neutral.shape)
        gain = np.polyval(meta["vignette_polynomial"], np.sqrt(r2))
        sensor = neutral / gain[..., None]
        target = neutral**0.8
        encoded = np.where(
            target <= 0.0031308, target * 12.92, 1.055 * target ** (1 / 2.4) - 0.055
        )
        jpeg = Image.fromarray(np.rint(encoded * 255).astype(np.uint8))
        result = probe.vignette_fit(sensor, jpeg, raw, meta)
        self.assertLess(
            result["complete"]["heldout_log_rmse"],
            result["none"]["heldout_log_rmse"] / 5,
        )
        self.assertAlmostEqual(result["fitted_strength"]["alpha"], 1, delta=0.06)


if __name__ == "__main__":
    unittest.main()
