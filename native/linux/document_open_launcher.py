"""Source-only Linux composition; installation and presenter choice are operator work."""

from dataclasses import dataclass
import json
import os
import selectors
import subprocess
import sys
import threading
import time

LIMIT = 65_536
LOOPBACK = "http://127.0.0.1:4173"


class Failure(Exception):
    """A deliberately non-sensitive failure category."""


@dataclass(frozen=True)
class Output:
    status: int
    stdout: bytes
    stderr: bytes


def native_absolute(path):
    """Preserve Unix byte paths, including Python's surrogateescape argv."""
    value = os.fsencode(path)
    if not value.startswith(b"/") or b"\0" in value:
        raise Failure("invalid document or installation path")
    return value


def run_bounded(argv, *, timeout=45, environment=None):
    """Bound both pipes and wall time; cleanup signals only our direct child."""
    process = subprocess.Popen(
        argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, env={} if environment is None else environment,
        close_fds=True,
    )
    captured = [bytearray(), bytearray()]
    try:
        with selectors.DefaultSelector() as selector:
            for index, pipe in enumerate((process.stdout, process.stderr)):
                os.set_blocking(pipe.fileno(), False)
                selector.register(pipe, selectors.EVENT_READ, index)
            deadline = time.monotonic() + timeout
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Failure("controller deadline")
                exited = process.poll() is not None
                # After child exit, drain only currently available bytes. A
                # descendant retaining a pipe must not make us wait for EOF.
                events = selector.select(0 if exited else min(remaining, 0.05))
                for key, _ in events:
                    chunk = os.read(key.fileobj.fileno(), 4096)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output = captured[key.data]
                    if len(output) + len(chunk) > LIMIT:
                        raise Failure("controller output limit")
                    output.extend(chunk)
                if exited and not events:
                    break
        return Output(process.returncode, bytes(captured[0]), bytes(captured[1]))
    finally:
        try:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        raise Failure("controller cleanup deadline") from None
        finally:
            process.stdout.close()
            process.stderr.close()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result


def reject_constant(_):
    raise ValueError("non-JSON constant")


def validated_destination(output):
    if output.status != 0:
        raise Failure("controller failed")
    if len(output.stdout) > LIMIT or len(output.stderr) > LIMIT:
        raise Failure("invalid controller response")
    try:
        envelope = json.loads(
            output.stdout.decode("utf-8"), object_pairs_hook=unique_object,
            parse_constant=reject_constant,
        )
        result = envelope["result"]
        if not (
            envelope["schemaVersion"] == "migration-stream/assisted-local-runtime/cli/v1"
            and envelope["ok"] is True
            and result["schemaVersion"] == "migration-stream/assisted-local-runtime/status/v2"
            and result["classification"] == "OWNED"
            and result["listenAddress"] == "127.0.0.1:4173"
            and result["url"] == LOOPBACK
        ):
            raise ValueError("envelope")
    except (ValueError, KeyError, TypeError, RecursionError):
        raise Failure("invalid controller response") from None
    # Never forward a URL (or token) read from the response.
    return LOOPBACK


class DocumentOpenLauncher:
    """One retained adapter; the controller retains cross-process ownership guards."""

    def __init__(self, trusted_runtime_root, browser, runner=None):
        if not callable(browser):
            raise Failure("installation presenter is required")
        self.executable = native_absolute(trusted_runtime_root).rstrip(b"/") + (
            b"/libexec/migration-stream-local-runtime"
        )
        self.browser = browser
        self.runner = run_bounded if runner is None else runner
        self.selected_path = None
        self.lock = threading.Lock()

    def open(self, documents):
        if not self.lock.acquire(blocking=False):
            raise Failure("open already in progress")
        try:
            if len(documents) != 1:
                raise Failure("exactly one document required")
            path = native_absolute(documents[0])
            if self.selected_path is not None and path != self.selected_path:
                raise Failure("different document already selected")
            output = self.runner([self.executable, b"open", b"--", path])
            destination = validated_destination(output)
            self.selected_path = path
            try:
                self.browser(destination)
            except Exception:
                raise Failure("browser presentation failed") from None
        finally:
            self.lock.release()


def launch(arguments, *, trusted_runtime_root, browser, runner=None):
    """Explicit operator composition; the caller supplies all installation authority."""
    try:
        launcher = DocumentOpenLauncher(trusted_runtime_root, browser, runner)
        launcher.open(arguments)
        return 0
    except (Failure, OSError):
        # Do not expose selected paths, child output, URLs, or capabilities.
        print("Migration Stream could not open the object.", file=sys.stderr)
        return 1
