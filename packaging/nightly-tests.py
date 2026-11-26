#!/usr/bin/env python3
"""Run the synthetic nightly packaging suites together; no app build, signing or network.

The Linux archive and caller contract suite is packaging/nightly-release-tests.py; the macOS
scripts run here on any POSIX host against generated fixtures and stand-in Apple tools. The Windows
suites (packaging/windows/nightly-*-tests.ps1) need pwsh and run in their own job.
"""
import importlib.util
from pathlib import Path
import unittest


def main() -> int:
    here = Path(__file__).resolve().parent
    suite = unittest.TestSuite()
    for name, relative in (
        ("light_icon_suite", "nightly-icons-tests.py"),
        ("light_plist_suite", "macos/nightly_plist_tests.py"),
        ("light_release_dispatch_suite", "nightly-release-tests.py"),
        ("light_mac_sign_suite", "macos/nightly_sign_tests.py"),
    ):
        spec = importlib.util.spec_from_file_location(name, here / relative)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite.addTests(unittest.defaultTestLoader.loadTestsFromModule(module))
    return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
