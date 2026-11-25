#!/usr/bin/env python3
"""The Nightly icon derivation: only the field's share changes; foreground and alpha do not.

Runs on any host with the committed renders (no resvg, iconutil or network). The committed outputs
are re-derived with `--check`, so a hand-edited or stale icon fails here.
"""
import importlib.util
import subprocess
import sys
from pathlib import Path
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("light_nightly_icons", HERE / "nightly-icons.py")
ICONS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ICONS)
ASSETS = HERE.parent / "assets" / "app-icon"


def pixels(path):
    return ICONS.read_png(path.read_bytes())


class Downsample(unittest.TestCase):
    def test_transparent_padding_never_darkens_a_blocks_colour(self):
        # A 2x2 block of one amber pixel and three transparent ones: the visible colour stays the amber
        # it was (PNG RGB is straight, not premultiplied) and only the coverage drops to a quarter.
        master = bytearray([154, 102, 16, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        self.assertEqual(tuple(ICONS.downsample(2, 2, master, 1)), (154, 102, 16, 64))

    def test_a_fully_transparent_block_stays_transparent(self):
        self.assertEqual(tuple(ICONS.downsample(2, 2, bytearray(16), 1)), (0, 0, 0, 0))

    def test_two_colours_mix_by_coverage(self):
        master = bytearray([0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0])
        self.assertEqual(tuple(ICONS.downsample(2, 2, master, 1)), (128, 128, 128, 128))


class Recolour(unittest.TestCase):
    def test_the_field_moves_and_the_figure_does_not(self):
        for colour, expected in (
            (ICONS.BACKGROUND, ICONS.NIGHTLY_BACKGROUND),
            (ICONS.PAPER, ICONS.PAPER),
            (ICONS.INK, ICONS.INK),
        ):
            self.assertEqual(tuple(round(c + ICONS.background_weight(colour) * (expected[i] - ICONS.BACKGROUND[i])) for i, c in enumerate(colour)), expected)

    def test_a_paper_and_ink_blend_keeps_its_exact_colour(self):
        # A real anti-aliased blend of the two foreground colours: no field share at all.
        for t in (0.0, 0.25, 0.5, 0.75, 1.0):
            blend = tuple(round(t * ICONS.INK[i] + (1 - t) * ICONS.PAPER[i]) for i in range(3))
            self.assertEqual(ICONS.background_weight(blend), 0.0, blend)

    def test_a_field_and_paper_blend_keeps_its_paper_and_loses_the_old_field(self):
        blend = tuple(round(0.5 * ICONS.BACKGROUND[i] + 0.5 * ICONS.PAPER[i]) for i in range(3))
        self.assertAlmostEqual(ICONS.background_weight(blend), 0.5, delta=0.02)

    @unittest.skipUnless((ASSETS / "lightcraft-nightly.png").exists(), "the Nightly icons are not derived yet")
    def test_the_committed_png_changes_only_the_field_and_keeps_alpha(self):
        width, height, stable = pixels(ASSETS / "hicolor/256x256/apps/ai.storyteller.lightcraft.png")
        derived_width, derived_height, nightly = pixels(ASSETS / "lightcraft-nightly.png")
        self.assertEqual((width, height), (derived_width, derived_height))
        for i in range(0, len(stable), 4):
            self.assertEqual(stable[i + 3], nightly[i + 3], "alpha must be preserved exactly")
            colour = (stable[i], stable[i + 1], stable[i + 2])
            expected = tuple(min(255, max(0, round(colour[j] + ICONS.background_weight(colour) * (ICONS.NIGHTLY_BACKGROUND[j] - ICONS.BACKGROUND[j])))) for j in range(3))
            self.assertEqual(tuple(nightly[i:i + 3]), expected)

    @unittest.skipUnless((ASSETS / "lightcraft-nightly-1024.png").exists(), "the Nightly icons are not derived yet")
    def test_the_padded_master_keeps_its_margin_and_its_frame_sizes(self):
        # The macOS master is the padded drawing: its transparent margin must stay transparent, and
        # no frame is ever upscaled from a smaller render.
        width, height, nightly = pixels(ASSETS / "lightcraft-nightly-1024.png")
        self.assertEqual((width, height), (1024, 1024))
        self.assertEqual(nightly[3], 0, "the padded Mac margin is transparent")
        self.assertEqual(nightly[-1], 0, "the padded Mac margin is transparent")
        ico = ICONS.read_ico((ASSETS / "lightcraft-nightly.ico").read_bytes())
        self.assertEqual(sorted(ico), list(ICONS.ICO_FRAMES))
        slots = ICONS.read_icns((ASSETS / "lightcraft-nightly.icns").read_bytes())
        for kind, size in ICONS.ICNS_EXISTING.items():
            self.assertEqual(ICONS.read_png(slots[kind])[0], size)
        for size, kind in ICONS.ICNS_FROM_MASTER.items():
            self.assertEqual(ICONS.read_png(slots[kind])[0], size)

    @unittest.skipUnless((ASSETS / "lightcraft-nightly.icns").exists(), "the Nightly icons are not derived yet")
    def test_the_committed_icons_are_the_committed_derivation(self):
        result = subprocess.run([sys.executable, str(HERE / "nightly-icons.py"), "--check"], capture_output=True, text=True, timeout=900)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
