"""Synthetic sweep-tool checks, with no corpus or external renderer required.

Run: python3 -m unittest discover -s docs/raw -p 'test_*.py'
"""

import copy
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import numpy as np
from PIL import Image

import coverage_render_sweep as sweep


def gradient():
    values = np.tile(np.arange(20, 220, 4, dtype=np.uint8), (32, 1))
    return Image.fromarray(np.repeat(values[..., None], 3, axis=-1))


class Measurements(unittest.TestCase):
    def test_preview_tiff_uses_structure_without_srgb_colour_metrics(self):
        image = gradient()
        with mock.patch.object(sweep, "score", side_effect=AssertionError("colour scorer must not run")):
            measured = sweep.measure_pair(image, image, True)
        self.assertEqual(set(measured), {"r"})
        self.assertAlmostEqual(measured["r"], 1)
        with self.assertRaisesRegex(ValueError, "constant structure"):
            sweep.measure_pair(Image.new("RGB", (32, 32)), image, True)
        with self.assertRaisesRegex(ValueError, "aspect ratio"):
            sweep.measure_pair(image, Image.new("RGB", (1, 11)), True)
        with self.assertRaisesRegex(ValueError, "dimensions"):
            sweep.measure_pair(image, Image.new("RGB", (0, 1)), True)

    def test_display_references_keep_the_existing_colour_scorer(self):
        image = gradient()
        self.assertEqual(sweep.measure_pair(image, image, False), sweep.score(image, image))

    def test_structure_gate_ignores_colour_errors_but_keeps_correlation(self):
        base = dict(r=0.9, mean_dl=0, outer_mae_dl=0)
        candidate = dict(metric_kind="structure_luma", r=0.95, mean_dl=100, outer_mae_dl=100)
        self.assertFalse(sweep.metrics_regressed(candidate, base))
        self.assertTrue(sweep.metrics_regressed(dict(candidate, r=0.8), base))
        self.assertTrue(sweep.metrics_regressed(dict(candidate, metric_kind="lab_lstar"), base))
        self.assertTrue(sweep.metrics_regressed(dict(candidate, metric_kind="lab_lstar", mean_dl=0), base))

    def test_baseline_inputs_are_checked_before_any_deltas(self):
        current = dict(
            file_sha256="a" * 64,
            reference_sha256="b" * 64,
            reference_size=[50, 32],
            reference="embedded JPEG",
            rotation=0,
            metric_kind="lab_lstar",
            pixel_sha256="c" * 64,
            r=0.95,
            mean_dl=0.1,
            outer_mae_dl=0.2,
        )
        changes = dict(
            file_sha256="d" * 64,
            reference_sha256="e" * 64,
            reference_size=[32, 50],
            reference="exiftool:PreviewTIFF",
            rotation=180,
            metric_kind="structure_luma",
        )
        for key, wrong in changes.items():
            for value in (wrong, None):
                with self.subTest(key=key, value=value):
                    record = copy.deepcopy(current)
                    base = dict(current, **{key: value})
                    with self.assertRaisesRegex(ValueError, "baseline " + key + ".*sample.nef"):
                        sweep.compare_record("sample.nef", record, base)
                    self.assertNotIn("delta_r", record)
        base = json.loads(json.dumps(current))
        del base["metric_kind"]  # Existing display-reference baseline records remain compatible.
        sweep.compare_record("sample.nef", current, base)
        self.assertEqual(current["delta_r"], 0)
        self.assertEqual(current["delta_abs_mean_dl"], 0)
        self.assertFalse(current["pixels_changed"])

    def test_old_tiff_metrics_and_nonfinite_baselines_are_rejected(self):
        record = dict(
            file_sha256="a", reference_sha256="b", reference_size=[50, 32],
            reference="exiftool:PreviewTIFF", rotation=0, metric_kind="structure_luma",
            pixel_sha256="c", r=1,
        )
        old = dict(record)
        del old["metric_kind"]
        with self.assertRaisesRegex(ValueError, "regenerate baseline"):
            sweep.compare_record("sample.fff", record, old)
        for value in (float("nan"), float("inf"), None, "1"):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "baseline r"):
                sweep.compare_record("sample.fff", record, dict(record, r=value))
        with self.assertRaisesRegex(ValueError, "baseline record missing"):
            sweep.compare_record("sample.fff", record, None)


class Resume(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        previous = Path.cwd()
        self.addCleanup(os.chdir, previous)
        os.chdir(self.temp.name)
        self.cli = Path("renderer")
        self.cli.write_bytes(b"synthetic renderer identity")
        self.output = Path("elsewhere/output")
        self.fields = ["file", "sha256", "reference", "notes"]
        Path("docs/raw").mkdir(parents=True)

    def manifest(self, rows):
        with Path("docs/raw/coverage-sweep.tsv").open("w") as stream:
            writer = csv.DictWriter(stream, fieldnames=self.fields, delimiter="\t")
            writer.writeheader()
            writer.writerows(rows)

    def invoke(self, extra=()):
        with mock.patch("sys.argv", ["sweep", "--cli", str(self.cli), "--out", str(self.output), *extra]), mock.patch("builtins.print"):
            sweep.main()

    def prepare_sample(self):
        raw = Path("corpus/raw/sample.nef")
        raw.parent.mkdir(parents=True)
        raw.write_bytes(b"original synthetic input")
        self.manifest([dict(file=raw.name, sha256=hashlib.sha256(raw.read_bytes()).hexdigest(), reference="embedded JPEG", notes="")])
        self.output.mkdir(parents=True)
        cache_stem = raw.stem + "-" + hashlib.sha256(raw.read_bytes()).hexdigest()
        gradient().save(self.output / (cache_stem + "-reference.png"))
        return self.output / (cache_stem + ".png")

    def test_missing_lens_database_parents_are_created_with_an_external_output(self):
        self.manifest([])
        self.assertFalse(Path("corpus").exists())
        self.invoke()
        self.assertTrue(Path("corpus/nikon-lens/empty-lensdb").is_dir())

    def test_readable_png_rejects_partial_corrupt_and_non_png_files(self):
        path = Path("cached.png")
        buffer = io.BytesIO()
        gradient().save(buffer, format="PNG")
        data = buffer.getvalue()
        self.assertFalse(sweep.readable_png(path))
        corrupt = bytearray(data)
        corrupt[29] ^= 1  # Corrupt the IHDR CRC while preserving the pixel stream.
        for bad in (b"", data[:24], data[:-12], bytes(corrupt)):
            with self.subTest(length=len(bad)):
                path.write_bytes(bad)
                self.assertFalse(sweep.readable_png(path))
        gradient().save(path, format="JPEG")
        self.assertFalse(sweep.readable_png(path))
        path.write_bytes(data)
        self.assertTrue(sweep.readable_png(path))

    def test_valid_cached_render_is_reused_and_empty_render_is_rebuilt(self):
        output = self.prepare_sample()
        gradient().save(output)
        with mock.patch.object(sweep.subprocess, "run") as run:
            self.invoke()
        run.assert_not_called()
        output.write_bytes(b"")
        def render(args, **kwargs):
            gradient().save(args[args.index("-o") + 1])
        with mock.patch.object(sweep.subprocess, "run", side_effect=render) as run:
            self.invoke()
        run.assert_called_once()
        self.assertTrue(sweep.readable_png(output))

    def test_changed_input_cannot_reuse_same_stem_pixels_or_references(self):
        output = self.prepare_sample()
        gradient().save(output)
        self.invoke()
        baseline = self.output / "scores.json"
        for name in ("sample.nef", "sample.dng"):
            with self.subTest(name=name):
                raw = Path("corpus/raw") / name
                raw.write_bytes(("replacement input " + name).encode())
                digest = hashlib.sha256(raw.read_bytes()).hexdigest()
                self.manifest([dict(file=name, sha256=digest, reference="embedded JPEG", notes="")])
                image = gradient().transpose(Image.Transpose.FLIP_LEFT_RIGHT)
                buffer = io.BytesIO()
                image.save(buffer, format="PNG")
                def render(args, **kwargs):
                    image.save(args[args.index("-o") + 1])
                def extract(args, **kwargs):
                    return buffer.getvalue() if "-JpgFromRaw" in args else b""
                with mock.patch.object(sweep.subprocess, "run", side_effect=render) as run, mock.patch.object(sweep.subprocess, "check_output", side_effect=extract) as extract_call:
                    self.invoke()
                run.assert_called_once()
                self.assertEqual(extract_call.call_count, 3)
                self.assertEqual(Path(run.call_args.args[0][run.call_args.args[0].index("-o") + 1]).name, "sample-" + digest + ".png")
                self.assertTrue((self.output / ("sample-" + digest + ".log")).is_file())
                reference = self.output / ("sample-" + digest + "-reference.png")
                with Image.open(reference) as cached:
                    self.assertEqual(cached.tobytes(), image.tobytes())
                record = json.loads(baseline.read_text())["records"][name]
                self.assertEqual(record["file_sha256"], digest)
                self.assertEqual(record["reference_sha256"], hashlib.sha256(image.tobytes()).hexdigest())
                with mock.patch.object(sweep.subprocess, "run") as run, mock.patch.object(sweep.subprocess, "check_output") as extract_call:
                    self.invoke()
                run.assert_not_called()
                extract_call.assert_not_called()
        self.output = Path("candidate")
        with mock.patch.object(sweep.subprocess, "run", side_effect=render) as run, mock.patch.object(sweep.subprocess, "check_output") as extract_call:
            self.invoke(["--compare", str(baseline)])
        run.assert_called_once()
        extract_call.assert_not_called()
        record = json.loads((self.output / "scores.json").read_text())["records"][name]
        self.assertFalse(record["pixels_changed"])
        self.assertEqual(record["delta_r"], 0)

    def test_preview_tiff_run_records_structure_only(self):
        output = self.prepare_sample()
        gradient().save(output)
        raw = Path("corpus/raw/sample.nef")
        self.manifest([dict(file=raw.name, sha256=hashlib.sha256(raw.read_bytes()).hexdigest(), reference="exiftool:PreviewTIFF", notes="")])
        with mock.patch.object(sweep, "score", side_effect=AssertionError("colour scorer must not run")):
            self.invoke()
        record = json.loads((self.output / "scores.json").read_text())["records"][raw.name]
        self.assertEqual(record["metric_kind"], "structure_luma")
        self.assertAlmostEqual(record["r"], 1)
        self.assertNotIn("mean_dl", record)
        self.assertNotIn("outer_mae_dl", record)

    def test_a_baseline_with_different_input_files_is_rejected(self):
        self.manifest([])
        baseline = Path("baseline.json")
        baseline.write_text(json.dumps(dict(records={"another.nef": {}})))
        with self.assertRaisesRegex(ValueError, "baseline input files differ"):
            self.invoke(["--compare", str(baseline)])


if __name__ == "__main__":
    unittest.main()
