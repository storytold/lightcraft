"""Failure-path and cancellation checks for the LightCraft inventory wrapper."""
import importlib.util
import io
import hashlib
import stat
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SOURCE = Path(__file__).resolve().with_name('generate-cargo-sources.py')
spec = importlib.util.spec_from_file_location('inventory_wrapper', SOURCE)
wrapper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrapper)

# Deliberate sabotage for checking that the cancellation tests are meaningful.
# Run with LIGHTCRAFT_FLATPAK_TEST_BREAK_KILL=1: the process-group kill becomes
# a no-op and the cancellation/timeout tests must fail, proving they observe
# real liveness instead of relying on teardown.
BREAK_KILL = os.environ.get('LIGHTCRAFT_FLATPAK_TEST_BREAK_KILL') == '1'
if BREAK_KILL:
    wrapper._kill_generator = lambda process: None

# The process-group tests use POSIX killpg plus /proc to tell an unreaped
# zombie descendant from a running one; both are Linux-specific here, so they
# are skipped explicitly rather than pretending to be portable POSIX.
LINUX_ONLY = sys.platform.startswith('linux')
LINUX_SKIP = 'process-group cancellation is verified on Linux (/proc liveness, killpg)'

# Process identity recorded with each PID, so teardown never kills a recycled
# PID: /proc/<pid>/stat starttime is monotonic per PID on Linux.
PYTHON_IDENTITY = (
    'def write_identity(path, pid):\n'
    '    stat = pathlib.Path(f"/proc/{pid}/stat").read_text()\n'
    '    starttime = stat.rsplit(")", 1)[1].split()[19]\n'
    '    path.write_text(f"{pid} {starttime}")\n'
)

# A descendant that keeps writing into the cache until it is killed.
DESCENDANT = (
    'import os, pathlib, sys, time\n'
    + PYTHON_IDENTITY +
    'heartbeat, marker, pidfile = (pathlib.Path(arg) for arg in sys.argv[1:])\n'
    'write_identity(pidfile, os.getpid())\n'
    'with heartbeat.open("ab", buffering=0) as handle:\n'
    '    for _ in range(1000):\n'
    '        handle.write(b"x")\n'
    '        time.sleep(0.02)\n'
    'marker.write_bytes(b"descendant survived")\n'
)


def detached_generator():
    """Fake generator: starts a heartbeating descendant, then waits."""
    return (
        'import os, pathlib, subprocess, sys, time\n'
        + PYTHON_IDENTITY +
        f'CHILD = {DESCENDANT!r}\n'
        'root = pathlib.Path(sys.argv[-1]).parent\n'
        'write_identity(root / "generator.pid", os.getpid())\n'
        'child_script = root / "descendant.py"\n'
        'child_script.write_text(CHILD)\n'
        'subprocess.Popen([sys.executable, str(child_script), str(root / "cache" / "heartbeat"), str(root / "cache" / "after-timeout"), str(root / "descendant.pid")])\n'
        'deadline = time.monotonic() + 10\n'
        'while time.monotonic() < deadline:\n'
        '    heartbeat = root / "cache" / "heartbeat"\n'
        '    if heartbeat.exists() and heartbeat.stat().st_size:\n'
        '        break\n'
        '    time.sleep(0.01)\n'
        'else:\n'
        '    raise SystemExit("descendant never wrote a heartbeat")\n'
        '(root / "ready").write_bytes(b"handshake")\n'
        'time.sleep(30)\n'
    ).encode()


# Stands in for `python3 generate-cargo-sources.py` so a real signal can be
# delivered to the wrapper while the detached generator is running. The fake
# payload keeps the test offline and the timeout long enough to be cancelled.
DRIVER = '''\
import importlib.util
import os
from pathlib import Path
import sys

source, fixture, lock, output, cache = sys.argv[1:6]
spec = importlib.util.spec_from_file_location("inventory_wrapper", source)
wrapper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrapper)
if os.environ.get("LIGHTCRAFT_FLATPAK_TEST_BREAK_KILL") == "1":
    wrapper._kill_generator = lambda process: None
payload = Path(fixture).read_bytes()
wrapper.download_upstream = lambda: payload
wrapper.patched_upstream = lambda data: data
wrapper.GENERATOR_TIMEOUT_SECONDS = 300
sys.argv = ["generate", lock, "-o", output, "--cache-dir", cache]
try:
    raise SystemExit(wrapper.main())
except KeyboardInterrupt:
    Path(fixture).with_name("interrupted").write_bytes(b"KeyboardInterrupt")
    raise SystemExit(130)
'''


class InventoryFailures(unittest.TestCase):
    def test_download_accepts_verified_bytes(self):
        payload = b'verified fixture'
        with patch.object(wrapper, 'urlopen', return_value=io.BytesIO(payload)), patch.object(wrapper, 'UPSTREAM_SHA256', hashlib.sha256(payload).hexdigest()):
            self.assertEqual(wrapper.download_upstream(), payload)

    def test_patch_applies_both_changes_and_checks_result_hash(self):
        source = '\n'.join(old for _, old, _ in wrapper.PATCHES).encode()
        expected = '\n'.join(new for _, _, new in wrapper.PATCHES).encode()
        with patch.object(wrapper, 'PATCHED_SHA256', hashlib.sha256(expected).hexdigest()):
            self.assertEqual(wrapper.patched_upstream(source), expected)
        with patch.object(wrapper, 'PATCHED_SHA256', '0' * 64):
            with self.assertRaises(SystemExit):
                wrapper.patched_upstream(source)

    def test_success_atomically_replaces_inventory_with_owner_only_file(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            lock = root / 'Cargo.lock'
            lock.write_text('version = 4\n')
            output = root / 'sources.json'
            output.write_bytes(b'old inventory')
            payload = b'[{"type": "archive", "url": "https://example.invalid/source"}]'
            def generate(*args, **kwargs):
                temporary = Path(kwargs['temp_output'])
                self.assertEqual(output.read_bytes(), b'old inventory')
                if os.name == 'posix':
                    self.assertEqual(stat.S_IMODE(temporary.stat().st_mode), 0o600)
                temporary.write_bytes(payload)
                return 0
            args = ['generate', str(lock), '-o', str(output), '--cache-dir', str(root / 'cache')]
            with patch.object(sys, 'argv', args), patch.object(wrapper, 'download_upstream', return_value=b'fixture'), patch.object(wrapper, 'patched_upstream', return_value=b'fixture'), patch.object(wrapper, 'run_generator', side_effect=generate):
                self.assertEqual(wrapper.main(), 0)
            self.assertEqual(output.read_bytes(), payload)
            if os.name == 'posix':
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            self.assertEqual(list(root.glob('.sources.json.*.tmp')), [])

    def test_download_rejects_changed_bytes(self):
        with patch.object(wrapper, 'urlopen', return_value=io.BytesIO(b'changed upstream')):
            with self.assertRaisesRegex(SystemExit, 'checksum mismatch'):
                wrapper.download_upstream()

    def test_download_rejects_excessive_size(self):
        with patch.object(wrapper, 'urlopen', return_value=io.BytesIO(b'x' * 1_000_001)):
            with self.assertRaisesRegex(SystemExit, 'unexpectedly large'):
                wrapper.download_upstream()

    def test_patch_refuses_unrecognized_source(self):
        with self.assertRaisesRegex(SystemExit, 'matched 0 times'):
            wrapper.patched_upstream(b'not the expected generator')

    def test_cache_cannot_be_inside_checkout_or_target(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            for cache in (root / 'repo', root / 'repo/subdir', root / 'target', root / 'target/subdir'):
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(root / 'target')}):
                    with self.assertRaises(SystemExit):
                        wrapper.ensure_outside_source(cache, root / 'repo')

    def test_relative_target_uses_generator_working_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            # The wrapper can be invoked elsewhere; its child runs in repo_root.
            elsewhere = root / 'elsewhere/deeper'
            elsewhere.mkdir(parents=True)
            previous = Path.cwd()
            try:
                os.chdir(elsewhere)
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': '../target'}):
                    with self.assertRaisesRegex(SystemExit, 'outside CARGO_TARGET_DIR'):
                        wrapper.ensure_outside_source(root / 'target/cache', root / 'repo')
            finally:
                os.chdir(previous)

    def test_timed_out_generator_preserves_inventory(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            lock = root / 'Cargo.lock'
            lock.write_text('version = 4\n')
            output = root / 'sources.json'
            output.write_bytes(b'original inventory')
            generator = b'import pathlib, sys, time\npathlib.Path(sys.argv[-1]).write_bytes(b"partial")\ntime.sleep(30)\n'
            args = ['generate', str(lock), '-o', str(output), '--cache-dir', str(root / 'cache')]
            with patch.object(sys, 'argv', args), patch.object(wrapper, 'download_upstream', return_value=generator), patch.object(wrapper, 'patched_upstream', side_effect=lambda data: data), patch.object(wrapper, 'GENERATOR_TIMEOUT_SECONDS', 0.2):
                with self.assertRaisesRegex(SystemExit, 'existing inventory was not changed'):
                    wrapper.main()
            self.assertEqual(output.read_bytes(), b'original inventory')
            self.assertEqual(list(root.glob('.sources.json.*.tmp')), [])

    def test_generator_signal_handlers_are_restored(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            script = root / 'generator.py'
            script.write_text(
                'import pathlib, sys\n'
                'pathlib.Path(sys.argv[-1]).write_text("[]")\n'
            )
            lock = root / 'Cargo.lock'
            lock.write_text('version = 4\n')
            temp_output = root / 'sources.json'
            watched = (signal.SIGINT, signal.SIGTERM)
            before = {number: signal.getsignal(number) for number in watched}
            returncode = wrapper.run_generator(
                script=script,
                cargo_lock=lock,
                temp_output=str(temp_output),
                cwd=root,
                environment=os.environ.copy(),
            )
            after = {number: signal.getsignal(number) for number in watched}
            self.assertEqual(returncode, 0)
            self.assertEqual(temp_output.read_text(), '[]')
            self.assertEqual(before, after)

    def test_signal_after_wait_is_not_lost_during_handler_restore(self):
        root = self._new_temp_root()
        script = root / 'generator.py'
        script.write_text(
            'import pathlib, sys\n'
            'pathlib.Path(sys.argv[-1]).write_text("[]")\n'
        )
        lock = root / 'Cargo.lock'
        lock.write_text('version = 4\n')
        temp_output = root / 'sources.json'
        watched = (signal.SIGINT, signal.SIGTERM)
        before = {number: signal.getsignal(number) for number in watched}
        real_signal = signal.signal
        calls = {'count': 0}
        captured = {}

        def restore_with_signal(signum, handler):
            calls['count'] += 1
            if calls['count'] <= len(watched):
                captured['handler'] = handler
            elif calls['count'] == len(watched) + 1:
                # The child has been reaped and run_generator is restoring the
                # handlers. Invoke the still-installed run.cancel handler now:
                # it records the cancellation after run_generator's old
                # snapshot point, so only the post-restoration read can report
                # it. This is deterministic, unlike racing a real signal.
                captured['handler'](signal.SIGINT, None)
            return real_signal(signum, handler)

        environment = os.environ.copy()
        output_path = str(temp_output)
        try:
            with patch.object(wrapper.signal, 'signal', side_effect=restore_with_signal):
                with self.assertRaises(wrapper._Cancelled) as caught:
                    wrapper.run_generator(
                        script=script,
                        cargo_lock=lock,
                        temp_output=output_path,
                        cwd=root,
                        environment=environment,
                    )
        finally:
            for number in watched:
                real_signal(number, before[number])
        self.assertEqual(caught.exception.signum, signal.SIGINT)

    @unittest.skipUnless(LINUX_ONLY, LINUX_SKIP)
    def test_timed_out_generator_stops_descendants(self):
        root = self._new_temp_root()
        lock = root / 'Cargo.lock'
        lock.write_text('version = 4\n')
        output = root / 'sources.json'
        output.write_bytes(b'original inventory')
        args = ['generate', str(lock), '-o', str(output), '--cache-dir', str(root / 'cache')]
        try:
            with patch.object(sys, 'argv', args), patch.object(wrapper, 'download_upstream', return_value=detached_generator()), patch.object(wrapper, 'patched_upstream', side_effect=lambda data: data), patch.object(wrapper, 'GENERATOR_TIMEOUT_SECONDS', 3.0):
                with self.assertRaisesRegex(SystemExit, 'existing inventory was not changed'):
                    wrapper.main()

            # Assert liveness and a frozen heartbeat before any teardown kill,
            # so a broken group kill cannot be masked by the test's cleanup.
            self.assertTrue((root / 'ready').exists())
            self._assert_generator_session_gone(root)
            self.assertEqual(output.read_bytes(), b'original inventory')
            self.assertEqual(list(root.glob('.sources.json.*.tmp')), [])
        except BaseException:
            self._preserve_failure_evidence(root, 'timeout')
            raise

    @unittest.skipUnless(LINUX_ONLY, LINUX_SKIP)
    def test_sigint_cancellation_stops_generator_session(self):
        self._assert_cancellation(signal.SIGINT, expected_returncode=130)

    @unittest.skipUnless(LINUX_ONLY, LINUX_SKIP)
    def test_sigterm_cancellation_stops_generator_session(self):
        self._assert_cancellation(signal.SIGTERM, expected_returncode=-signal.SIGTERM)

    def _assert_cancellation(self, number, *, expected_returncode):
        root = self._new_temp_root()
        driver, output = self._start_generation_driver(root)
        try:
            driver.send_signal(number)
            returncode = driver.wait(timeout=30)
            self.assertEqual(returncode, expected_returncode, (root / 'driver.err').read_text())
            if number == signal.SIGINT:
                # KeyboardInterrupt reached the wrapper's caller after cleanup.
                self.assertEqual((root / 'interrupted').read_bytes(), b'KeyboardInterrupt')
            else:
                # SIGTERM kept its default terminating action.
                self.assertFalse((root / 'interrupted').exists())

            # Assert the process group is dead and the heartbeat has stopped
            # before any teardown kill. Cleanup is registered with addCleanup,
            # so it runs after these assertions (also when they fail).
            self._assert_generator_session_gone(root)
            self.assertEqual(output.read_bytes(), b'original inventory')
            self.assertEqual(list(root.glob('.sources.json.*.tmp')), [])
        except BaseException:
            self._preserve_failure_evidence(root, f'cancel-{int(number)}')
            raise

    def _new_temp_root(self):
        temp = tempfile.TemporaryDirectory(prefix='lightcraft-flatpak-test-')
        self.addCleanup(temp.cleanup)
        root = Path(temp.name).resolve()
        # Registered after temp.cleanup, so addCleanup's LIFO order stops any
        # surviving test processes before their evidence directory disappears.
        self.addCleanup(self._stop_pids, root)
        return root

    def _start_generation_driver(self, root):
        lock = root / 'Cargo.lock'
        lock.write_text('version = 4\n')
        output = root / 'sources.json'
        output.write_bytes(b'original inventory')
        fixture = root / 'generator-fixture.py'
        fixture.write_bytes(detached_generator())
        driver_path = root / 'driver.py'
        driver_path.write_text(DRIVER)
        environment = {
            key: value for key, value in os.environ.items() if key != 'CARGO_TARGET_DIR'
        }
        stdout = (root / 'driver.out').open('wb')
        stderr = (root / 'driver.err').open('wb')
        self.addCleanup(stdout.close)
        self.addCleanup(stderr.close)
        driver = subprocess.Popen(
            [
                sys.executable, '-B', str(driver_path), str(SOURCE), str(fixture),
                str(lock), str(output), str(root / 'cache'),
            ],
            cwd=str(root),
            env=environment,
            stdout=stdout,
            stderr=stderr,
        )
        self.addCleanup(self._stop_driver, driver, root)
        deadline = time.monotonic() + 20
        while not (root / 'ready').exists():
            if driver.poll() is not None:
                self.fail(
                    'driver exited before the generator was ready:\n'
                    + (root / 'driver.err').read_text()
                )
            if time.monotonic() > deadline:
                self.fail('driver did not start the generator descendant')
            time.sleep(0.02)
        return driver, output

    def _stop_driver(self, driver, root):
        if driver.poll() is None:
            driver.kill()
        try:
            driver.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        self._stop_pids(root)

    @staticmethod
    def _read_identity(root, name):
        try:
            pid_text, starttime = (root / name).read_text().split()
            return int(pid_text), starttime
        except (OSError, ValueError):
            return None

    @staticmethod
    def _identity_is_running(pid, starttime):
        """True only for the recorded process, never for a recycled PID."""
        try:
            stat = Path(f'/proc/{pid}/stat').read_text()
        except OSError:
            return False
        fields = stat.rsplit(')', 1)[1].split()
        return len(fields) > 19 and fields[0] not in ('Z', 'X') and fields[19] == starttime

    @classmethod
    def _stop_pids(cls, root):
        for name in ('generator.pid', 'descendant.pid'):
            identity = cls._read_identity(root, name)
            if identity is None:
                continue
            pid, starttime = identity
            if not cls._identity_is_running(pid, starttime):
                continue
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        # Let teardown observe the kill before the evidence directory is
        # removed; identity checks make this a no-op for unrelated PIDs.
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            if not any(
                identity is not None and cls._identity_is_running(*identity)
                for identity in (cls._read_identity(root, name) for name in ('generator.pid', 'descendant.pid'))
            ):
                break
            time.sleep(0.02)

    @staticmethod
    def _preserve_failure_evidence(root, label):
        destination = Path(tempfile.mkdtemp(prefix=f'lightcraft-flatpak-{label}-'))
        for path in root.rglob('*'):
            if not path.is_file():
                continue
            target = destination / path.relative_to(root)
            try:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, target)
            except OSError:
                pass
        print(f'failure evidence preserved in {destination}', file=sys.stderr)

    def _assert_generator_session_gone(self, root):
        for name in ('generator.pid', 'descendant.pid'):
            identity = self._read_identity(root, name)
            self.assertIsNotNone(identity, f'{name} was not recorded')
            pid, starttime = identity
            deadline = time.monotonic() + 2
            while self._identity_is_running(pid, starttime) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertFalse(
                self._identity_is_running(pid, starttime),
                f'{name} process {pid} is still running',
            )

        heartbeat = (root / 'cache' / 'heartbeat').read_bytes()
        self.assertTrue(heartbeat)
        time.sleep(0.5)
        self.assertEqual((root / 'cache' / 'heartbeat').read_bytes(), heartbeat)
        self.assertFalse((root / 'cache' / 'after-timeout').exists())

    def test_failed_or_invalid_generation_preserves_inventory(self):
        for code, payload in [(17, b'partial'), (0, b''), (0, b'invalid json'), (0, b'[]'), (0, b'{}')]:
            with self.subTest(code=code, payload=payload), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve()
                lock = root / 'Cargo.lock'
                lock.write_text('version = 4\n')
                output = root / 'sources.json'
                output.write_bytes(b'original inventory')
                def generate(*args, **kwargs):
                    Path(kwargs['temp_output']).write_bytes(payload)
                    return code
                args = ['generate', str(lock), '-o', str(output), '--cache-dir', str(root / 'cache')]
                with patch.object(sys, 'argv', args), patch.object(wrapper, 'download_upstream', return_value=b'fixture'), patch.object(wrapper, 'patched_upstream', return_value=b'fixture'), patch.object(wrapper, 'run_generator', side_effect=generate):
                    if code:
                        self.assertEqual(wrapper.main(), code)
                    else:
                        with self.assertRaises((SystemExit, ValueError)):
                            wrapper.main()
                self.assertEqual(output.read_bytes(), b'original inventory')
                self.assertEqual(list(root.glob('.sources.json.*.tmp')), [])


if __name__ == '__main__':
    unittest.main(verbosity=2)
