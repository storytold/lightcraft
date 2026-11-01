"""Checks that a calibrate_noise.py file is validated and drives train.py's synthetic noise.

    python -m unittest discover -s tools/denoise-train -p "test_*.py"

Needs numpy and torch (the training environment); no data, no graphics card.
"""

import contextlib
import io
import json
import math
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import torch  # noqa: E402

import calibrate_noise as C  # noqa: E402
import train as T  # noqa: E402

PLANE_A = {"R": 1.1, "G1": 1.0, "G2": 1.0, "B": 1.25}  # relative shot-noise slope per plane


def rawdump_output(iso, a, b, make="Testcam", model="One"):
    """What `rawdump noise` prints for a photo whose flattest blocks follow variance = a*m + b exactly."""
    means = [math.exp(math.log(1e-3) + i * (math.log(0.6) - math.log(1e-3)) / 19) for i in range(20)]
    planes = []
    for pl, rel in PLANE_A.items():
        bins = [{"mean": m, "n": 100, "p10": (a * rel * m + b) * C.P10_BIAS} for m in means]
        planes.append({"plane": pl, "bins": bins})
    return {"iso": iso, "make": make, "model": model, "planes": planes}


def calibration(entries):
    """A calibration file as calibrate_noise.py writes it, from measurements made by rawdump_output."""
    return {"model": "variance = a * mean + b", "cameras": C.build_calibration(entries)}


class Tmp:
    def __init__(self):
        self.dir = tempfile.TemporaryDirectory()

    def write(self, doc, name="cal.json"):
        path = os.path.join(self.dir.name, name)
        with open(path, "w", encoding="utf8") as f:
            f.write(doc if isinstance(doc, str) else json.dumps(doc))
        return path


class CalibrateFit(unittest.TestCase):
    def test_build_calibration_recovers_the_noise_curve_per_iso_and_plane(self):
        cal = C.build_calibration([rawdump_output(800, 1.7e-4, 2e-8), rawdump_output(3200, 6.6e-4, 1.25e-6), None])
        self.assertEqual(sorted(cal["Testcam One"]), ["3200", "800"])
        e = cal["Testcam One"]["3200"]
        self.assertEqual(e["photos"], 1)
        for pl, rel in PLANE_A.items():
            self.assertAlmostEqual(e[pl]["a"] / (6.6e-4 * rel), 1.0, delta=1e-6)
            self.assertAlmostEqual(e[pl]["b"] / 1.25e-6, 1.0, delta=1e-3)

    def test_build_calibration_reads_a_one_pass_iterator(self):
        cal = C.build_calibration(iter([rawdump_output(3200, 6.6e-4, 1.25e-6), None]))
        self.assertEqual(cal["Testcam One"]["3200"]["photos"], 1)

    def test_a_fit_below_zero_read_noise_is_refitted_with_none(self):
        # variance falling under a*m in the shadows would fit b < 0: the file gets b = 0 and a positive slope
        pts = [(m, 1e-4 * m - 2e-7) for m in (0.01, 0.02, 0.05, 0.1, 0.3, 0.6)]
        a, b = C.fit(pts)
        self.assertEqual(b, 0.0)
        self.assertGreater(a, 0.0)


class CalibrateCommandLine(unittest.TestCase):
    def test_the_helpers_utf8_output_is_read_as_utf8_whatever_the_locale(self):
        # a camera name with "Á" (UTF-8 C3 81: 0x81 is undefined in cp1252, so a locale decode fails or garbles)
        tmp = Tmp()
        try:
            d = tmp.dir.name
            out = rawdump_output(3200, 6.6e-4, 1.25e-6, make="Cámara", model="Á1")
            raw_bytes = json.dumps(out, ensure_ascii=False).encode("utf-8")
            helper = tmp.write(f"import sys\nsys.stdout.buffer.write({raw_bytes!r})\n", "helper.py")
            if os.name == "nt":
                exe = tmp.write(f'@"{sys.executable}" "{helper}" %*\r\n', "rawdump.cmd")
            else:
                exe = tmp.write(f'#!/bin/sh\nexec "{sys.executable}" "{helper}" "$@"\n', "rawdump")
                os.chmod(exe, 0o755)
            os.makedirs(os.path.join(d, "raws"))
            open(os.path.join(d, "raws", "a.dng"), "w").close()
            cal = os.path.join(d, "cal.json")
            argv = ["calibrate_noise.py", "--rawdump", exe, "--out", cal, os.path.join(d, "raws")]
            with mock.patch.object(sys, "argv", argv), contextlib.redirect_stdout(io.StringIO()), \
                    mock.patch.object(C.subprocess, "run", wraps=C.subprocess.run) as run, \
                    mock.patch.object(C, "build_calibration", wraps=C.build_calibration) as build:
                self.assertIsNone(C.main())
            self.assertEqual([e["camera"] for e in T.load_calibration(cal)], ["Cámara Á1"])
            # decoded as UTF-8 by request, not by a UTF-8 locale happening to be set (CodeRabbit on #2932)
            self.assertEqual(run.call_args.kwargs.get("encoding"), "utf-8")
            # the measurements are folded in as they arrive, never all held (CodeRabbit on #2929)
            (arg,), _ = build.call_args
            self.assertNotIsInstance(arg, (list, tuple))
            self.assertIs(iter(arg), arg, "an iterator, read once")
        finally:
            tmp.dir.cleanup()


class LoadCalibration(unittest.TestCase):
    def setUp(self):
        self.tmp = Tmp()
        self.good = calibration([rawdump_output(3200, 6.6e-4, 1.25e-6)])

    def tearDown(self):
        self.tmp.dir.cleanup()

    def test_a_calibrate_noise_file_loads_in_plane_order(self):
        (e,) = T.load_calibration(self.tmp.write(self.good))
        self.assertEqual((e["camera"], e["iso"], e["filled"]), ("Testcam One", 3200, []))
        for i, pl in enumerate(T.PLANES):
            self.assertAlmostEqual(e["a"][i] / (6.6e-4 * PLANE_A[pl]), 1.0, delta=1e-6)

    def bad(self, doc, words):
        with self.assertRaises(T.CalibrationError) as cm:
            T.load_calibration(self.tmp.write(doc))
        self.assertIn(words, str(cm.exception))

    def entry(self, **planes):
        doc = json.loads(json.dumps(self.good))
        doc["cameras"]["Testcam One"]["3200"].update(planes)
        return doc

    def test_unreadable_empty_or_wrong_files_are_errors(self):
        with self.assertRaises(T.CalibrationError) as cm:
            T.load_calibration(os.path.join(self.tmp.dir.name, "missing.json"))
        self.assertIn("can't be read", str(cm.exception))
        self.bad("", "not JSON")
        self.bad("[1, 2]", 'no "cameras"')
        self.bad({"cameras": {}}, '"cameras" is empty')
        self.bad({"cameras": {"Testcam One": {}}}, "has no ISOs")
        self.bad({"cameras": {"Testcam One": {"high": {"G1": {"a": 1e-4, "b": 0}}}}}, "not a whole number")
        self.bad({"cameras": {"Testcam One": {"0": {"G1": {"a": 1e-4, "b": 0}}}}}, "must be above 0")
        self.bad({"cameras": {"Testcam One": {"100": {"photos": 3}}}}, "no plane has a measurement")

    def test_missing_keys_nan_infinity_and_negative_values_are_errors(self):
        self.bad(self.entry(G1={"a": 1e-4}), 'needs both "a" and "b"')
        self.bad(self.entry(G3={"a": 1e-4, "b": 0}), "unknown keys ['G3']")
        self.bad(json.dumps(self.entry()).replace(str(self.good["cameras"]["Testcam One"]["3200"]["B"]["b"]), "NaN"),
                 "NaN, not a finite number")
        self.bad(self.entry(R={"a": float("inf"), "b": 0}), "holds Infinity, not a finite number")
        self.bad(json.dumps(self.entry(R={"a": 12345.0, "b": 0})).replace("12345.0", "1e999"), "inf, not a finite number")
        self.bad(self.entry(R={"a": -1e-4, "b": 1e-6}), "R a is -0.0001; it must be above 0")
        self.bad(self.entry(R={"a": 0, "b": 1e-6}), "R a is 0.0; it must be above 0")
        self.bad(self.entry(B={"a": 1e-4, "b": -1e-7}), "B b is -1e-07; it must be at least 0")
        self.bad(self.entry(G2={"a": "1e-4", "b": 0}), "not a number")
        self.bad(self.entry(G2={"a": True, "b": 0}), "not a number")
        # finite as a Python float, but infinity or 0 in the float32 noise tables (CodeRabbit on #2929)
        self.bad(self.entry(R={"a": 1e40, "b": 0}), "outside the float32 range")
        self.bad(self.entry(R={"a": 1e-3, "b": 1e40}), "outside the float32 range")
        self.bad(self.entry(R={"a": 1e-50, "b": 0}), "outside the float32 range")

    def test_a_plane_the_calibration_left_out_takes_the_others_mean_and_says_so(self):
        doc = self.entry()
        del doc["cameras"]["Testcam One"]["3200"]["G2"]
        (e,) = T.load_calibration(self.tmp.write(doc))
        self.assertEqual(e["filled"], ["G2"])
        self.assertAlmostEqual(e["a"][2], (e["a"][0] + e["a"][1] + e["a"][3]) / 3)
        lines = T.NoiseModel([e], source="cal.json").describe()
        self.assertTrue(any("G2 not measured" in ln for ln in lines), lines)

    def test_a_calibration_with_no_read_noise_anywhere_is_an_error(self):
        e = {"camera": "c", "iso": 100, "a": [1e-4] * 4, "b": [0.0] * 4, "filled": []}
        with self.assertRaises(T.CalibrationError):
            T.NoiseModel([e])

    def test_with_every_patch_measured_no_read_noise_relation_is_fitted_or_reported(self):
        # (CodeRabbit on #2929: at --cal-share 1 the relation is never used, so b = 0 everywhere is fine)
        e = {"camera": "c", "iso": 100, "a": [1e-4] * 4, "b": [0.0] * 4, "filled": []}
        m = T.NoiseModel([e], share=1.0, source="cal.json")
        self.assertIn("every patch uses a measured entry exactly", m.describe()[0])
        self.assertNotIn("fitted relation", m.describe()[0])
        self.assertIsNone(m.summary()["read_c"])
        a4, b4 = m.sample(torch.full((8,), 3e-3), torch.Generator().manual_seed(1))
        self.assertTrue(torch.all(a4 == torch.tensor(1e-4)) and torch.all(b4 == 0))

    def test_a_cal_share_outside_0_to_1_is_an_error(self):
        for share in (-0.1, 1.5, float("nan")):
            with self.assertRaises(T.CalibrationError):
                T.NoiseModel(share=share)


class NoiseFromCalibration(unittest.TestCase):
    def entry(self, iso, a, b):
        return {"camera": "Testcam One", "iso": iso, "a": [a * PLANE_A[p] for p in T.PLANES], "b": [b] * 4,
                "filled": []}

    def test_measured_noise_has_the_calibrated_variance_per_plane(self):
        a, b, x = 6.6e-4, 1.25e-6, 0.2
        model = T.NoiseModel([self.entry(3200, a, b)], share=1.0)
        g = torch.Generator().manual_seed(5)
        a4, b4 = model.sample(torch.full((16,), 1e-3), g)  # the drawn a is replaced by the measured entry
        clean = torch.full((16, 4, 64, 64), x)
        noisy = T.add_noise(clean, a4, b4, g, row=False)
        var = (noisy - clean).var(dim=(0, 2, 3))
        for i, pl in enumerate(T.PLANES):
            want = a * PLANE_A[pl] * x + b
            self.assertAlmostEqual(float(var[i]) / want, 1.0, delta=0.03, msg=pl)

    def test_half_the_patches_take_a_measured_entry_and_the_rest_the_fitted_relation(self):
        model = T.NoiseModel([self.entry(800, 1.7e-4, 2e-8), self.entry(3200, 6.6e-4, 1.25e-6)], share=0.5)
        g = torch.Generator().manual_seed(7)
        drawn = torch.full((4000,), 3.3e-3)  # an a no entry has
        a4, b4 = model.sample(drawn, g)
        measured = (a4[:, 1] != drawn)
        self.assertAlmostEqual(float(measured.float().mean()), 0.5, delta=0.03)
        self.assertTrue(set(a4[measured, 1].tolist()) <= {torch.tensor(1.7e-4).item(), torch.tensor(6.6e-4).item()})
        # the rest: b = c a^2 x lognormal jitter (median 1) + floor, c fitted to the two entries
        c = math.exp((math.log(2e-8 / (1.7e-4 * 1.0875) ** 2) + math.log(1.25e-6 / (6.6e-4 * 1.0875) ** 2)) / 2)
        self.assertAlmostEqual(model.c / c, 1.0, delta=1e-6)
        ratio = (b4[~measured, 0] - T.READ_FLOOR) / (model.c * 3.3e-3 ** 2)
        self.assertAlmostEqual(float(ratio.median()), 1.0, delta=0.1)

    def test_without_a_calibration_the_noise_is_the_built_in_relation_unchanged(self):
        g1, g2 = torch.Generator().manual_seed(3), torch.Generator().manual_seed(3)
        a = torch.exp(torch.linspace(math.log(1e-5), math.log(2.5e-2), 64))
        _, b4 = T.NoiseModel().sample(a, g1)
        # the formula train.py used before calibrations were read
        old = 1.25e-6 * (a / 6.6e-4) ** 2 * torch.exp(torch.randn(a.shape, generator=g2) * 0.9) + 2e-8
        self.assertTrue(torch.allclose(b4[:, 0], old, rtol=1e-6, atol=0))
        self.assertTrue(torch.equal(b4[:, 0], b4[:, 3]))
        self.assertIn("built-in EOS R5 C", T.NoiseModel().describe()[0])

    def test_make_batch_uses_the_calibration(self):
        g = torch.Generator().manual_seed(11)
        rgb, valid = torch.full((4, 3, 32, 32), 0.3), torch.ones(4, 32, 32)
        model = T.NoiseModel([self.entry(3200, 6.6e-4, 1.25e-6)], share=1.0)
        with mock.patch.object(model, "sample", wraps=model.sample) as spy:
            noisy, target, v, k = T.make_batch(rgb, valid, g, noise=model)
        self.assertEqual(spy.call_count, 1)
        self.assertEqual(tuple(noisy.shape), (4, 4, 16, 16))
        self.assertTrue(torch.isfinite(noisy).all())


class TrainCommandLine(unittest.TestCase):
    def test_zero_checkpoint_validation_or_log_intervals_are_refused_at_start(self):
        # (CodeRabbit on #2929: `step % 0` stopped the run after its first step)
        tmp = Tmp()
        try:
            out = os.path.join(tmp.dir.name, "run")
            for flag in ("--ckpt", "--val-every", "--log-every"):
                argv = ["train.py", "--data", tmp.dir.name, "--out", out, "--device", "cpu", flag, "0"]
                err = io.StringIO()
                with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(err), \
                        self.assertRaises(SystemExit) as cm:
                    T.main()
                self.assertEqual(cm.exception.code, 2)
                self.assertIn(f"{flag} must be at least 1", err.getvalue())
            self.assertFalse(os.path.exists(out))
        finally:
            tmp.dir.cleanup()

    def test_a_bad_calibration_stops_train_py_before_anything_is_written(self):
        tmp = Tmp()
        try:
            path = tmp.write({"cameras": {"Testcam One": {"3200": {"G1": {"a": float("nan"), "b": 0}}}}})
            out = os.path.join(tmp.dir.name, "run")
            err = io.StringIO()
            argv = ["train.py", "--data", tmp.dir.name, "--out", out, "--noise-cal", path, "--device", "cpu"]
            with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(err):
                self.assertEqual(T.main(), 2)
            self.assertIn("not a finite number", err.getvalue())
            self.assertFalse(os.path.exists(out))
        finally:
            tmp.dir.cleanup()


if __name__ == "__main__":
    unittest.main()
