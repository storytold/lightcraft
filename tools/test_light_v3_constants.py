"""Synthetic publication checks. Run with python3 -m unittest discover -s tools."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

import light_v3_constants as constants


class SanitizeTests(unittest.TestCase):
    def test_counts_distinct_photos_for_each_group_and_band(self):
        fit = json.loads((constants.FITS / "contrast.json").read_text())
        # Three repeated pairs from one photo must not qualify as three photos.
        def row(photo, amount, bands):
            return dict(photo=photo, a_tgt=amount, a_ref=0, long_edge=800,
                        encoded_error=0.25, cells={b: {"error": 0.5} for b in bands})

        fit["check"] = [row("synthetic-a", 20, constants.BANDS) for _ in range(3)]
        fit["check"] += [row(p, 40, constants.BANDS) for p in ("synthetic-a", "synthetic-b")]
        fit["check"] += [row(p, 60, constants.BANDS[:2]) for p in ("synthetic-a", "synthetic-b", "synthetic-c")]
        fit["check"] += [row("synthetic-a", 60, constants.BANDS) for _ in range(3)]
        fit["check"] += [row("synthetic-b", 60, constants.BANDS[:3])]
        fit["tune"] = fit["check"]
        with tempfile.TemporaryDirectory() as temp:
            source, dest = Path(temp) / "synthetic.json", Path(temp) / "public.json"
            source.write_text(json.dumps(fit))
            with contextlib.redirect_stdout(io.StringIO()):
                constants.sanitize(SimpleNamespace(fit=source, slider="contrast", o=dest))
            public = json.loads(dest.read_text())
        self.assertEqual(public["params"], fit["params"])
        self.assertEqual(public["check_photos"], 3)
        for split in ("tune", "check"):
            rows = public["median_errors"][split]
            self.assertEqual([r["amount"] for r in rows], [60])
            self.assertEqual(rows[0]["photos"], 3)
            self.assertEqual(set(rows[0]["band_error"]), set(constants.BANDS[:2]))
            self.assertEqual(rows[0]["band_photos"], {b: 3 for b in constants.BANDS[:2]})

    def test_single_photo_fit_has_no_error_aggregates(self):
        fit = json.loads((constants.FITS / "contrast.json").read_text())
        fit["tune"] = [dict(photo="synthetic-a", a_tgt=20, a_ref=0, long_edge=800,
                            encoded_error=0.25, cells={})]
        fit["check"] = []
        with tempfile.TemporaryDirectory() as temp:
            source, dest = Path(temp) / "synthetic.json", Path(temp) / "public.json"
            source.write_text(json.dumps(fit))
            with contextlib.redirect_stdout(io.StringIO()):
                constants.sanitize(SimpleNamespace(fit=source, slider="contrast", o=dest))
            public = json.loads(dest.read_text())
        self.assertEqual(public["median_errors"], {"tune": [], "check": []})
        self.assertIsNone(public["cost"])

    def test_committed_aggregates_have_at_least_three_photos(self):
        for path in sorted(constants.FITS.glob("*.json")):
            fit = json.loads(path.read_text())
            for rows in fit["median_errors"].values():
                for row in rows:
                    with self.subTest(fit=path.name, amount=row["amount"], edge=row["long_edge"]):
                        self.assertGreaterEqual(row.get("photos", row["pairs"]), 3)
                        self.assertEqual(set(row["band_error"]), set(row.get("band_photos", {})))
                        self.assertTrue(all(n >= 3 for n in row.get("band_photos", {}).values()))


if __name__ == "__main__":
    unittest.main()
