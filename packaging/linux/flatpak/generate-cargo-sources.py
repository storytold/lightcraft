#!/usr/bin/env python3
"""Regenerate cargo-sources.json for the from-source Flatpak manifest.

The upstream flatpak-cargo-generator is downloaded from a pinned commit and
verified before use; the full script is intentionally not vendored here. Two
small, reviewed patches are applied in memory:

1. Key cargo source replacements by the full source URL (including ?rev=...)
   instead of only the repository URL, so two different revisions of the same
   repository stay distinct.
2. Include Cargo.lock's [patch.unused] entries, so crates such as egui_demo_lib
   are vendored even though they are not in the resolved package graph.

This only produces and validates the source inventory; a full Flatpak SDK
build is still required. See README.md in this directory for details.

The generator runs with stdin connected to /dev/null and is capped at
GENERATOR_TIMEOUT_SECONDS. On POSIX (the Flatpak build path) it starts in its
own session, so a timeout, Ctrl-C (SIGINT) or SIGTERM terminates the generator
and its git descendants without touching any other process; the recorded signal
is replayed with the handler that was installed before generation started, so
the wrapper keeps its normal cancellation behaviour. On Windows only the direct
generator process is terminated; descendants are not tree-killed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
from urllib.request import urlopen

UPSTREAM_COMMIT = "9d86d336a39b9af38b86f114b6ce5820fe260f91"
UPSTREAM_URL = (
    "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/"
    f"{UPSTREAM_COMMIT}/cargo/flatpak-cargo-generator.py"
)
UPSTREAM_SHA256 = "0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380"
PATCHED_SHA256 = "1e7c61d98d18e88156fb8cd4b91f526e04a6c504b185ae7b12f775ec1dd48512"

# Allow cold git caches while bounding stalled inventory regeneration.
GENERATOR_TIMEOUT_SECONDS = 30 * 60

# Signals that cancel regeneration without an exception of their own.
CANCELLATION_SIGNALS = (signal.SIGINT, signal.SIGTERM)

PATCHES = (
    (
        "preserve pinned source identities",
        '''    elif branch:
        assert len(branch) == 1
        cargo_vendored_entry[repo_url]["branch"] = branch[0]

    logging.info("Adding package %s from %s", name, repo_url)
''',
        '''    elif branch:
        assert len(branch) == 1
        cargo_vendored_entry[repo_url]["branch"] = branch[0]

    # Preserve distinct pinned source identities from the same repository.
    if rev or tag or branch:
        source_key = source.removeprefix("git+").split("#", 1)[0]
        cargo_vendored_entry = {source_key: cargo_vendored_entry[repo_url]}

    logging.info("Adding package %s from %s", name, repo_url)
''',
    ),
    (
        "include Cargo.lock patch.unused entries",
        '''    pkg_coros = [
        get_package_sources(p, cargo_lock, git_repos) for p in cargo_lock["package"]
    ]
''',
        '''    pkg_coros = [
        get_package_sources(p, cargo_lock, git_repos)
        for p in cargo_lock["package"] + cargo_lock.get("patch", {}).get("unused", [])
    ]
''',
    ),
)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def download_upstream() -> bytes:
    with urlopen(UPSTREAM_URL, timeout=60) as response:
        data = response.read(1_000_001)
    if len(data) > 1_000_000:
        raise SystemExit("error: upstream generator is unexpectedly large")
    actual = sha256(data)
    if actual != UPSTREAM_SHA256:
        raise SystemExit(
            "error: upstream generator checksum mismatch\n"
            f"  expected {UPSTREAM_SHA256}\n"
            f"  actual   {actual}\n"
            f"  url      {UPSTREAM_URL}"
        )
    return data


def patched_upstream(data: bytes) -> bytes:
    text = data.decode("utf-8")
    for name, old, new in PATCHES:
        count = text.count(old)
        if count != 1:
            raise SystemExit(
                f"error: patch {name!r} matched {count} times, expected exactly once; "
                "the pinned upstream script or patch set changed"
            )
        text = text.replace(old, new, 1)
    patched = text.encode("utf-8")
    actual = sha256(patched)
    if actual != PATCHED_SHA256:
        raise SystemExit(
            "error: patched generator checksum mismatch\n"
            f"  expected {PATCHED_SHA256}\n"
            f"  actual   {actual}"
        )
    return patched


def _kill_generator(process: subprocess.Popen) -> None:
    """Signal only our generator session; never waits or reaps.

    The reaped check is a plain ``returncode`` read: calling ``Popen.poll`` or
    ``Popen.kill`` would wait for the child, which is not safe inside a POSIX
    signal handler. On POSIX the generator is a session leader, so its PID is
    also the ID of a process group that contains no process we do not own.
    Killing that group stops git clones/fetches the generator started. On
    Windows only the direct generator process is killed; its descendants may
    survive.
    """
    if process.returncode is not None:
        return
    if os.name == "posix":
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            # The group is already gone; still try the direct child in case it
            # was not a group leader.
            try:
                os.kill(process.pid, signal.SIGKILL)
            except OSError:
                pass
    else:
        try:
            process.kill()
        except OSError:
            # The child exited between the timeout and the kill.
            pass


def _terminate_generator(process: subprocess.Popen) -> None:
    """Stop only our generator session and reap its leader.

    Only called outside signal handlers; the follow-up ``wait`` reaps the
    session leader so no zombie or live generator outlives the wrapper.
    ``_kill_generator`` skips a child that was already reaped, so this cannot
    signal a recycled PID.
    """
    _kill_generator(process)
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        print(
            f"warning: generator process {process.pid} did not exit after SIGKILL",
            file=sys.stderr,
        )


class _GeneratorRun:
    """State shared with the temporary cancellation signal handlers.

    Because the generator runs in its own session, Ctrl-C in the terminal or a
    SIGTERM sent to the wrapper no longer reaches it. These handlers record the
    signal and stop exactly that session without waiting; ``run_generator``
    then restores the previous handlers and reports the cancellation, so
    ``main`` can clean up before the signal is replayed with the caller's own
    behaviour. The wait that follows always reaps the leader.
    """

    def __init__(self) -> None:
        self.process = None
        self.received = None
        self.killed = False

    def cancel(self, signum, _frame) -> None:
        """Record the first SIGINT/SIGTERM and stop the generator's session."""
        if self.received is None:
            self.received = signum
        process = self.process
        # ``_kill_generator`` reads only ``returncode`` and uses raw POSIX
        # signals, so it is safe in a handler. The first signal does the kill;
        # later signals must not overwrite the recorded cancellation.
        if process is not None and not self.killed:
            self.killed = True
            _kill_generator(process)


class _Cancelled(Exception):
    """A cancellation signal arrived while the generator was running."""

    def __init__(self, signum: int) -> None:
        super().__init__(signum)
        self.signum = signum


def _replay_cancellation(signum: int) -> None:
    """Re-deliver a recorded cancellation through the restored handler.

    Called once the temporary handlers are restored and the temporary file and
    directory are gone. ``raise_signal`` invokes exactly the handler currently
    installed (``KeyboardInterrupt`` for the default SIGINT action, the default
    terminating action for SIGTERM, or ``SIG_IGN``), so no handler-kind
    branching is needed.
    """
    signal.raise_signal(int(signum))


def run_generator(
    *, script: Path, cargo_lock: Path, temp_output: str, cwd: Path, environment: dict
) -> int:
    """Run the upstream generator in its own session; return its exit status.

    A cancellation or any other error stops the generator's whole session
    before it is reported, so no detached generator or git process survives.
    """
    run = _GeneratorRun()
    previous_handlers = {}
    try:
        # Install the handlers before Popen: a signal that arrives while the
        # child is being spawned (or between Popen returning and the assignment
        # below) must not leave an unmanaged session behind. It is recorded,
        # and the newly created process is stopped as soon as its PID is known.
        # Installing inside the outer try means a partial installation is
        # restored even if a later signal.signal() call fails unexpectedly.
        for signum in CANCELLATION_SIGNALS:
            try:
                previous_handlers[signum] = signal.signal(signum, run.cancel)
            except (OSError, ValueError):
                # signal.signal() only works in the main thread of the main
                # interpreter; the exception cleanup below still stops the
                # detached generator on KeyboardInterrupt elsewhere.
                pass
        process = subprocess.Popen(
            [sys.executable, str(script), str(cargo_lock), "-o", temp_output],
            cwd=str(cwd),
            env=environment,
            stdin=subprocess.DEVNULL,
            start_new_session=os.name == "posix",
        )
        run.process = process
        if run.received is not None:
            _terminate_generator(process)
        try:
            process.wait(timeout=GENERATOR_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            _terminate_generator(process)
            if run.received is None:
                raise
            # A cancellation arrived while reaping the timed-out session; it
            # is reported below as the cancellation it is.
        except BaseException:
            # KeyboardInterrupt without a temporary handler (non-main thread)
            # or any other error: never leave the generator detached.
            _terminate_generator(process)
            raise
        returncode = process.returncode
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
    # Read the recorded signal after restoring the handlers: a signal that
    # arrives between the child being reaped and the restoration above is
    # recorded by run.cancel and must still be reported, not silently dropped.
    received = run.received
    if received is not None:
        raise _Cancelled(received)
    return returncode


def default_cache_dir() -> Path:
    base = os.environ.get("XDG_CACHE_HOME")
    if base:
        return Path(base) / "lightcraft-flatpak-cargo"
    return Path.home() / ".cache" / "lightcraft-flatpak-cargo"


def ensure_outside_source(cache: Path, repo_root: Path) -> None:
    cache = cache.resolve()
    if cache == repo_root or repo_root in cache.parents:
        raise SystemExit(f"error: cache directory must be outside the source checkout: {cache}")
    target = os.environ.get("CARGO_TARGET_DIR")
    if target:
        target_path = Path(target)
        # Cargo runs in repo_root, even when this wrapper was invoked elsewhere.
        if not target_path.is_absolute():
            target_path = repo_root / target_path
        target_path = target_path.resolve()
        if cache == target_path or target_path in cache.parents:
            raise SystemExit(f"error: cache directory must be outside CARGO_TARGET_DIR: {cache}")


def main() -> int:
    repo_root = Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "cargo_lock",
        nargs="?",
        default=str(repo_root / "Cargo.lock"),
        help="Cargo.lock to read (default: repository Cargo.lock)",
    )
    parser.add_argument(
        "-o",
        "--output",
        default=str(repo_root / "packaging/linux/flatpak/cargo-sources.json"),
        help="output path (default: packaging/linux/flatpak/cargo-sources.json)",
    )
    parser.add_argument(
        "--cache-dir",
        default=None,
        help=(
            "git clone cache root (default: $XDG_CACHE_HOME/lightcraft-flatpak-cargo, "
            "or ~/.cache/lightcraft-flatpak-cargo)"
        ),
    )
    args = parser.parse_args()

    cargo_lock = Path(args.cargo_lock).resolve()
    output = Path(args.output).resolve()
    cache = Path(args.cache_dir).resolve() if args.cache_dir else default_cache_dir().resolve()
    if not cargo_lock.is_file():
        raise SystemExit(f"error: Cargo.lock not found: {cargo_lock}")
    ensure_outside_source(cache, repo_root)
    cache.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)

    generator = patched_upstream(download_upstream())

    cancelled = None
    with tempfile.TemporaryDirectory(prefix="lightcraft-flatpak-generator-") as tmpdir:
        script = Path(tmpdir) / "flatpak-cargo-generator.py"
        script.write_bytes(generator)

        descriptor, temp_output = tempfile.mkstemp(
            prefix=f".{output.name}.", suffix=".tmp", dir=output.parent
        )
        os.close(descriptor)
        try:
            environment = os.environ.copy()
            environment["XDG_CACHE_HOME"] = str(cache)
            returncode = run_generator(
                script=script,
                cargo_lock=cargo_lock,
                temp_output=temp_output,
                cwd=repo_root,
                environment=environment,
            )
            if returncode != 0:
                return returncode
            if os.path.getsize(temp_output) == 0:
                raise SystemExit("error: generator produced an empty file")
            with open(temp_output, "r", encoding="utf-8") as handle:
                sources = json.load(handle)
            if not isinstance(sources, list) or not sources:
                raise SystemExit("error: generated sources are not a non-empty list")
            # Keep the owner-only mkstemp mode through the atomic replacement.
            os.replace(temp_output, output)
        except subprocess.TimeoutExpired as error:
            raise SystemExit(
                f"error: inventory generation exceeded {GENERATOR_TIMEOUT_SECONDS} seconds; "
                "the existing inventory was not changed"
            ) from error
        except _Cancelled as event:
            # Replayed below, after the temporary file and script directory are
            # removed, so cancellation leaves no leftovers.
            cancelled = event
        finally:
            if os.path.exists(temp_output):
                os.unlink(temp_output)

    if cancelled is not None:
        _replay_cancellation(cancelled.signum)
        raise SystemExit(f"error: generator cancelled by signal {cancelled.signum}")

    print(f"wrote {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
