"""Synthetic source proofs: never run the product controller or a browser."""

import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

import document_open_launcher as source


def owned(**changes):
    result = {
        "schemaVersion": "migration-stream/assisted-local-runtime/status/v2",
        "classification": "OWNED",
        "listenAddress": "127.0.0.1:4173",
        "url": source.LOOPBACK,
    }
    result.update(changes)
    return source.Output(0, json.dumps({
        "schemaVersion": "migration-stream/assisted-local-runtime/cli/v1",
        "ok": True, "result": result,
    }).encode(), b"")


class HandlerTests(unittest.TestCase):
    def test_exact_native_argv_and_fixed_controller(self):
        calls, pages = [], []
        with tempfile.TemporaryDirectory() as directory:
            root = os.fsencode(directory)
            path = root + "/--synthetic space;$(ignored)Δ.project".encode()
            descriptor = os.open(path, os.O_CREAT | os.O_WRONLY, 0o600)
            os.close(descriptor)
            adapter = source.DocumentOpenLauncher(
                root, pages.append, lambda argv: calls.append(argv) or owned(),
            )
            adapter.open([os.fsdecode(path)])
            adapter.open([path])
            self.assertEqual(calls, [[
                root + b"/libexec/migration-stream-local-runtime", b"open", b"--", path,
            ]] * 2)
            self.assertEqual(pages, [source.LOOPBACK] * 2)

    def test_non_utf8_native_argv_is_lossless_without_host_file_lookup(self):
        calls = []
        path = b"/synthetic/invalid-utf8-\xff.project"
        adapter = source.DocumentOpenLauncher(
            b"/synthetic", lambda _: None, lambda argv: calls.append(argv) or owned(),
        )
        adapter.open([os.fsdecode(path)])
        self.assertEqual(calls[0][-1], path)

    def test_invalid_inputs_never_run_or_present(self):
        for documents in ([], [b"/one", b"/two"], [b"relative"],
                          [b"file:///tmp/synthetic"], [b"/bad\0name"]):
            with self.subTest(documents=documents):
                adapter = source.DocumentOpenLauncher(
                    b"/synthetic", lambda _: self.fail("browser called"),
                    lambda _: self.fail("runner called"),
                )
                with self.assertRaises(source.Failure):
                    adapter.open(documents)

    def test_response_refusals_never_present_or_pin(self):
        invalid = [
            owned(classification="FOREIGN"), owned(classification="STOPPED"),
            owned(url="http://127.0.0.1:4173/?token=synthetic"),
            owned(url="https://127.0.0.1:4173"),
            owned(url="http://127.0.0.1:4173/"),
            owned(listenAddress="localhost:4173"),
            owned(schemaVersion="other"), source.Output(1, owned().stdout, b""),
            source.Output(0, b"[]", b""), source.Output(0, b"null", b""),
            source.Output(0, b"\xff", b""), source.Output(0, b"{} {}", b""),
            source.Output(0, owned().stdout.replace(b'"ok": true', b'"ok": 1'), b""),
            source.Output(0, owned().stdout.replace(b'"ok": true', b'"ok": false, "ok": true'), b""),
            source.Output(0, owned().stdout.replace(b'cli/v1', b'cli/v2'), b""),
            source.Output(0, owned().stdout, b"x" * (source.LIMIT + 1)),
            source.Output(0, b"x" * (source.LIMIT + 1), b""),
        ]
        for output in invalid:
            with self.subTest(output=output.stdout[:60]):
                adapter = source.DocumentOpenLauncher(
                    b"/synthetic", lambda _: self.fail("browser called"), lambda _: output,
                )
                with self.assertRaises(source.Failure):
                    adapter.open([b"/synthetic.project"])
                self.assertIsNone(adapter.selected_path)

    def test_browser_failure_pins_document_and_releases_lock(self):
        calls = []
        def browser(_):
            raise OSError("synthetic browser failure")
        adapter = source.DocumentOpenLauncher(
            b"/synthetic", browser, lambda argv: calls.append(argv) or owned(),
        )
        with self.assertRaisesRegex(source.Failure, "browser"):
            adapter.open([b"/one"])
        with self.assertRaisesRegex(source.Failure, "different document"):
            adapter.open([b"/two"])
        with self.assertRaisesRegex(source.Failure, "browser"):
            adapter.open([b"/one"])
        self.assertEqual(len(calls), 2)

    def test_busy_and_failed_controller_release(self):
        entered, release = threading.Event(), threading.Event()
        failures = []
        def runner(_):
            entered.set()
            if not release.wait(2):
                raise AssertionError("test release not delivered")
            raise OSError("synthetic runner failure")
        adapter = source.DocumentOpenLauncher(b"/synthetic", lambda _: None, runner)
        def invoke():
            try:
                adapter.open([b"/one"])
            except OSError:
                failures.append(True)
        worker = threading.Thread(target=invoke)
        worker.start()
        self.assertTrue(entered.wait(2))
        try:
            with self.assertRaisesRegex(source.Failure, "in progress"):
                adapter.open([b"/one"])
        finally:
            release.set()
            worker.join(2)
        self.assertFalse(worker.is_alive())
        self.assertEqual(failures, [True])
        self.assertTrue(adapter.lock.acquire(blocking=False))
        adapter.lock.release()

    def test_missing_presenter_is_inert_and_error_is_generic(self):
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            status = source.launch(
                ["/synthetic-private-name"], trusted_runtime_root=b"/synthetic",
                browser=None, runner=lambda _: self.fail("launched"),
            )
        self.assertEqual(status, 1)
        self.assertNotIn("synthetic-private-name", error.getvalue())

    def test_explicit_composition_uses_only_operator_bindings(self):
        calls, pages = [], []
        status = source.launch(
            [b"/synthetic.project"], trusted_runtime_root=b"/synthetic-runtime",
            browser=pages.append, runner=lambda argv: calls.append(argv) or owned(),
        )
        self.assertEqual(status, 0)
        self.assertEqual(calls, [[b"/synthetic-runtime/libexec/migration-stream-local-runtime",
                                 b"open", b"--", b"/synthetic.project"]])
        self.assertEqual(pages, [source.LOOPBACK])

    def test_uninstalled_templates_bind_interpreter_adapter_and_presenter(self):
        directory = Path(__file__).parent
        desktop = (directory / "MigrationStream.desktop.in").read_text()
        self.assertIn("Exec=@ABSOLUTE_PYTHON@ -I @ABSOLUTE_BOOTSTRAP@ %f\n", desktop)
        self.assertNotIn("MimeType=", desktop)
        template = (directory / "launcher_bootstrap.py.in").read_text()
        namespace = {"__name__": "synthetic_template_proof"}
        calls = []
        def runner(argv, **options):
            calls.append((argv, options))
            return source.Output(0, b"", b"")
        configured = template
        for binding, value in {
            "ABSOLUTE_ADAPTER_DIRECTORY": "/synthetic/adapter",
            "ABSOLUTE_RUNTIME_ROOT": "/synthetic/runtime",
            "ABSOLUTE_PRESENTER": "/synthetic/presenter",
        }.items():
            configured = configured.replace("@" + binding + "@", value)
        with patch.object(sys, "path", sys.path.copy()), patch.object(source, "run_bounded", runner):
            exec(compile(configured, "synthetic-bootstrap", "exec"), namespace)
            namespace["present"](source.LOOPBACK)
        self.assertEqual(namespace["RUNTIME_ROOT"], b"/synthetic/runtime")
        self.assertEqual(calls, [(([b"/synthetic/presenter", source.LOOPBACK]),
                                 {"timeout": 10, "environment": {}})])



class RunnerTests(unittest.TestCase):
    def helper(self, code, *, timeout=2):
        # Only this already-installed Python and inline synthetic helper execute.
        return source.run_bounded([os.fsencode(sys.executable), b"-c", code.encode()], timeout=timeout)

    def test_capture_status_and_empty_environment(self):
        output = self.helper(
            "import os,sys; os.write(1,b'synthetic-out'); os.write(2,b'synthetic-err'); "
            "sys.exit(0 if 'SYNTHETIC_SECRET' not in os.environ else 9)"
        )
        self.assertEqual(output, source.Output(0, b"synthetic-out", b"synthetic-err"))
        with patch.dict(os.environ, {"SYNTHETIC_SECRET": "never-forward"}):
            self.assertEqual(self.helper("import os; assert 'SYNTHETIC_SECRET' not in os.environ").status, 0)
        self.assertEqual(self.helper("raise SystemExit(7)").status, 7)

    def test_output_caps_both_streams(self):
        for descriptor in (1, 2):
            with self.subTest(descriptor=descriptor), self.assertRaisesRegex(source.Failure, "output limit"):
                self.helper(f"import os; os.write({descriptor}, b'x' * 131072)")

    def test_exact_output_limit_is_accepted(self):
        output = self.helper("import os; os.write(1,b'x'*65536); os.write(2,b'y'*65536)")
        self.assertEqual(output.status, 0)
        self.assertEqual(output.stdout, b"x" * source.LIMIT)
        self.assertEqual(output.stderr, b"y" * source.LIMIT)

    def test_deadline_cleans_owned_child(self):
        start = time.monotonic()
        with self.assertRaisesRegex(source.Failure, "deadline"):
            self.helper("import time; time.sleep(5)", timeout=0.1)
        self.assertLess(time.monotonic() - start, 4.5)

    def test_inherited_pipe_does_not_wait_for_descendant_eof(self):
        start = time.monotonic()
        output = self.helper(
            "import os,time; pid=os.fork(); "
            "time.sleep(0.8) if pid == 0 else os._exit(0)"
        )
        self.assertEqual(output.status, 0)
        self.assertLess(time.monotonic() - start, 0.7)


if __name__ == "__main__":
    unittest.main()
