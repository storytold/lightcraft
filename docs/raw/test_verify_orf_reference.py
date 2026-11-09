"""Regression tests for the black-box reference runner; no real raw files required."""

import contextlib
import hashlib
import io
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

import verify_orf_reference as reference


class ReferenceRunnerTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "synthetic executable uses a POSIX shebang")
    def test_relative_oracle_runs_from_the_scratch_directory(self):
        """Resolve a relative executable before switching cwd for sensor extraction."""
        with tempfile.TemporaryDirectory(prefix="lightcraft-orf-test-") as directory:
            root = pathlib.Path(directory)
            raw = root / "corpus" / "raw"
            raw.mkdir(parents=True)
            (raw / "sample.orf").write_bytes(b"fixture")
            fixtures = root / "cases.rs"
            input_hash = hashlib.sha256(b"fixture").hexdigest()
            pixel_hash = hashlib.sha256(bytes.fromhex("3412cdab")).hexdigest()
            fixtures.write_text(
                'const CASES: &[(&str, &str, (usize, usize), &str)] = &[\n'
                f'("sample.orf", "{input_hash}", (2, 1), "{pixel_hash}"),\n];\n'
                'fn example() { assert_eq!(name, "sample.orf"); }\n'
            )
            oracle = root / "oracle"
            oracle.write_text(
                f"#!{sys.executable}\n"
                "import pathlib, sys\n"
                "if len(sys.argv) > 1:\n"
                "    pathlib.Path(sys.argv[2] + '.pgm').write_bytes(\n"
                "        b'P5\\n2 1\\n65535\\n' + bytes.fromhex('1234abcd'))\n"
            )
            oracle.chmod(0o755)
            previous = pathlib.Path.cwd()
            try:
                os.chdir(root)
                with (
                    mock.patch.object(reference, "CASES", fixtures),
                    mock.patch.object(sys, "argv", ["verify_orf_reference.py", "--corpus", "corpus", "--oracle", "./oracle"]),
                    contextlib.redirect_stdout(io.StringIO()) as output,
                ):
                    reference.main()
                self.assertIn("All 1 reference sensors match", output.getvalue())
            finally:
                os.chdir(previous)


if __name__ == "__main__":
    unittest.main()
