#!/usr/bin/env python3
"""Pure injected state/graph tests. No process, real image or mount is used."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("macos_image_qualification", Path(__file__).with_name("macos-engineering-qualification.py"))
m = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(m)


class SyntheticRunner:
    def __init__(self, case):
        self.case, self.attached, self.commands = case, False, []
        self.attach_error, self.detach_error = False, False
        self.ambiguous, self.changed_ownership = False, False
        self.info_calls, self.cleanup_unresolved = 0, False

    def checkpoint(self):
        pass

    def __call__(self, arguments, **_bounds):
        self.commands.append([str(item) for item in arguments])
        self.case.assertEqual(arguments[0], m.HDIUTIL)
        operation = arguments[1]
        if operation == "info":
            self.info_calls += 1
            image = {"image-path": str(self.case.image), "system-entities": self.case.rows()}
            if self.changed_ownership and self.info_calls >= 3:
                image["system-entities"][0]["dev-entry"] = "/dev/disk99"
            return plistlib.dumps({"images": [image] if self.attached else []})
        if operation == "attach":
            self.case.assertTrue((self.case.work / "01-attach-pending.json").exists())
            self.case.assertEqual(arguments[2:8], ["-readonly", "-nobrowse", "-noautoopen", "-owners", "on", "-mountpoint"])
            self.attached = True
            if self.attach_error:
                raise m.QualificationError("synthetic uncertain attach")
            mount = self.case.mount
            m.a._copy_runtime(self.case.app, mount / m.p.APP_NAME, m.a._snapshot(self.case.app))
            rows = self.case.rows()
            if self.ambiguous:
                rows.append({"dev-entry": "/dev/disk99"})
            return plistlib.dumps({"system-entities": rows})
        self.case.assertEqual(operation, "detach")
        self.case.assertEqual(arguments, [m.HDIUTIL, "detach", "/dev/disk42"])
        if self.detach_error:
            raise m.QualificationError("synthetic detach refused")
        self.attached = False
        self.case.erase(self.case.mount / m.p.APP_NAME)
        return b""


class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="migration-stream-image-synthetic-")
        self.root = Path(self.temp.name).resolve()
        self.work, self.package = self.root / "work", self.root / "package"
        self.work.mkdir(); self.package.mkdir()
        self.app = self.package / m.p.APP_NAME
        runtime = self.app / "Contents/Resources/Runtime"
        (runtime / "libexec").mkdir(parents=True)
        (runtime / "libexec/migration-stream-local-runtime").write_bytes(b"FAKE never execute")
        (runtime / "libexec/migration-stream-local-runtime").chmod(0o555)
        (runtime / "data").write_bytes(b"fixed synthetic bytes\r\n\x1a")
        (runtime / "data").chmod(0o444)
        self.image = self.package / m.p.IMAGE_NAME
        self.image.write_bytes(b"FAKE not a disk image")
        self.image.chmod(0o444)
        for path, _, _ in os.walk(self.app):
            os.chmod(path, 0o555)
        self.mount = self.work / "attachment/volume"
        self.receipt = {"image": m.p._image_record(self.image),
                        "app": {"entries": m.a._contents(m.a._snapshot(self.app))},
                        "runtime": self.report(runtime)["result"]}
        self.receipt["runtime"].pop("bundleRoot")
        self.runner = SyntheticRunner(self)
        self.verified = []
        self.guard = mock.patch.object(m.subprocess, "Popen", side_effect=AssertionError("no process in pure qualification tests"))
        self.guard.start()

    def tearDown(self):
        self.guard.stop()
        for path, _, _ in os.walk(self.root, followlinks=False):
            os.chmod(path, 0o700)
        self.temp.cleanup()

    @staticmethod
    def erase(root):
        for path, _, _ in os.walk(root, followlinks=False):
            os.chmod(path, 0o700)
        shutil.rmtree(root)

    def rows(self):
        return [{"dev-entry": "/dev/disk42"}, {"dev-entry": "/dev/disk42s1"},
                {"dev-entry": "/dev/disk42s2", "mount-point": str(self.mount)}]

    @staticmethod
    def report(root):
        return {"schemaVersion": m.a.CLI_SCHEMA, "ok": True, "result": {
            "bundleRoot": str(root), "manifestSha256": "a" * 64, "productCommit": "b" * 40,
            "productVersion": "0.1.0", "sourceState": "CLEAN", "sourceSnapshotSha256": "c" * 64,
            "target": {"os": "macos", "architecture": "arm64", "targetTriple": "aarch64-apple-darwin", "minimumOsVersion": "11.0"},
            "launcherSha256": "d" * 64, "runtimeControllerSha256": "e" * 64,
            "integrityCliSha256": "f" * 64, "workbenchSha256": "1" * 64,
            "memberCount": 2, "totalMemberBytes": 64}}

    def validator(self, root):
        self.verified.append(root)
        return self.report(root)

    def qualify(self, **overrides):
        args = dict(validator=self.validator, mount_check=lambda *_args: (42, 7))
        args.update(overrides)
        return m.qualify_image(self.package, self.work, self.receipt, self.runner, **args)

    def test_exact_graph_modes_runtime_and_owned_detach(self):
        result = self.qualify()
        self.assertTrue(result["mountedGraphAndModesEqual"])
        self.assertTrue(result["privateCopyRuntimeVerified"])
        self.assertEqual(len(self.verified), 2)
        self.assertEqual([x[1] for x in self.runner.commands].count("attach"), 1)
        self.assertEqual([x[1] for x in self.runner.commands].count("detach"), 1)
        self.assertTrue((self.work / "04-detached.json").exists())
        self.assertEqual(list(self.mount.iterdir()), [])

    def test_uncertain_attach_never_guesses_detach(self):
        self.runner.attach_error = True
        with self.assertRaises(m.QualificationError): self.qualify()
        self.assertTrue((self.work / "01-attach-pending.json").exists())
        self.assertNotIn("detach", [x[1] for x in self.runner.commands])

    def test_ambiguous_devices_never_detach(self):
        self.runner.ambiguous = True
        with self.assertRaises(m.QualificationError): self.qualify()
        self.assertNotIn("detach", [x[1] for x in self.runner.commands])

    def test_fresh_ownership_change_prevents_detach(self):
        self.runner.changed_ownership = True
        with self.assertRaises(m.QualificationError): self.qualify()
        self.assertNotIn("detach", [x[1] for x in self.runner.commands])

    def test_changed_parent_before_mount_identity_prevents_detach(self):
        def changed_parent(_mount, parent, _expected):
            parent.rename(parent.with_name("retained-original-parent"))
            parent.mkdir()
            raise m.QualificationError("synthetic private parent replacement")
        with self.assertRaises(m.QualificationError): self.qualify(mount_check=changed_parent)
        self.assertNotIn("detach", [x[1] for x in self.runner.commands])
        self.assertFalse((self.work / "03-detach-pending.json").exists())

    def test_detach_failure_retains_unqualified_mount_state(self):
        self.runner.detach_error = True
        with self.assertRaises(m.QualificationError): self.qualify()
        self.assertTrue((self.work / "03-detach-pending.json").exists())
        self.assertFalse((self.work / "04-detached.json").exists())

    def test_runtime_rejection_still_detaches_only_owned_image(self):
        def reject(_root): raise m.QualificationError("synthetic Runtime refusal")
        with self.assertRaises(m.QualificationError): self.qualify(validator=reject)
        self.assertTrue((self.work / "04-detached.json").exists())
        self.assertEqual([x[1] for x in self.runner.commands].count("detach"), 1)

    def test_changed_bytes_mode_and_extra_nodes_refuse(self):
        for kind in ("bytes", "mode", "extra"):
            with self.subTest(kind=kind):
                expected = copy.deepcopy(self.receipt["app"]["entries"])
                target = self.app / "Contents/Resources/Runtime/data"
                target.chmod(0o644); target.write_bytes(b"fixed synthetic bytes\r\n\x1a"); target.chmod(0o444)
                if kind == "bytes":
                    target.chmod(0o644); target.write_bytes(b"changed"); target.chmod(0o444)
                elif kind == "mode": target.chmod(0o644)
                else:
                    self.app.chmod(0o755); (self.app / "extra").write_bytes(b"unlisted")
                with self.assertRaises(m.QualificationError): m.graph(self.app, expected)

    def test_image_digest_replay_refuses_before_attach(self):
        self.image.chmod(0o644); self.image.write_bytes(b"changed image")
        with self.assertRaises(m.QualificationError): self.qualify()
        self.assertEqual(self.runner.commands, [])

    def test_wrong_mount_extra_volume_duplicate_and_foreign_partition_refuse(self):
        for mutate in (lambda x: x[-1].update({"mount-point": "/Volumes/foreign"}),
                       lambda x: x.append({"dev-entry": "/dev/disk43s1", "mount-point": str(self.mount)}),
                       lambda x: x.append(x[0].copy()),
                       lambda x: x.append({"dev-entry": "/dev/disk43s1"}),
                       lambda x: x[-1].update({"dev-entry": "/dev/disk43s2"})):
            rows = self.rows(); mutate(rows)
            with self.assertRaises(m.QualificationError): m.entities({"system-entities": rows}, self.mount)

    def test_plist_and_receipt_caps_and_unknown_data_refuse(self):
        for data in (b"", b"not plist", b"x" * (m.MAX_PLIST + 1)):
            with self.assertRaises(m.QualificationError): m.parse_plist(data)
        path = self.work / "receipt.json"
        path.write_bytes(b'{"x":1,"x":2}')
        with self.assertRaises(m.QualificationError): m.read_json(path)
        path.write_bytes(b"x" * (m.MAX_PLIST + 1))
        with self.assertRaises(m.QualificationError): m.read_json(path)

    def test_no_reuse_of_mount_namespace(self):
        (self.work / "attachment").mkdir()
        with self.assertRaises(FileExistsError): self.qualify()
        self.assertEqual(self.runner.commands, [])

    def test_real_readonly_and_parent_identity_gate(self):
        parent = self.work / "plain"; parent.mkdir(); mount = parent / "volume"; mount.mkdir()
        pin = m.directory_id(parent)
        with self.assertRaises(m.QualificationError): m.mounted_identity(mount, parent, pin, readonly=False)
        with self.assertRaises(m.QualificationError): m.mounted_identity(mount, parent, pin, readonly=True)
        with self.assertRaises(m.QualificationError): m.mounted_identity(mount, parent, (1, 2), readonly=True)

    def test_expired_whole_runner_admits_no_child(self):
        clock = mock.Mock(side_effect=[0, m.WHOLE_SECONDS])
        runner = m.Runner(self.work, clock=clock)
        with self.assertRaises(m.QualificationError): runner([m.HDIUTIL, "info", "-plist"])
        self.assertEqual(runner.sequence, 0)

    def fake_child(self, output=b"", errors=b"", *, on_wait=None):
        class Child:
            def __init__(child):
                child.status = None
                child.terminated = child.killed = 0
                streams = []
                for data in (output, errors):
                    read, write = os.pipe(); os.write(write, data); os.close(write)
                    streams.append(os.fdopen(read, "rb"))
                child.stdout, child.stderr = streams
            def poll(child): return child.status
            def wait(child, *, timeout):
                if on_wait: on_wait()
                child.status = 0
                return 0
            def terminate(child): child.terminated += 1
            def kill(child): child.killed += 1
        return Child()

    def test_selector_failure_launches_no_child_and_setup_failure_cleans_child(self):
        start = mock.Mock()
        runner = m.Runner(self.work, popen=start)
        with mock.patch.object(m.selectors, "DefaultSelector", side_effect=OSError("synthetic selector refusal")):
            with self.assertRaises(OSError): runner([m.HDIUTIL, "info", "-plist"])
        start.assert_not_called()
        child = self.fake_child()
        runner = m.Runner(self.work, popen=lambda *_args, **_kwargs: child)
        with mock.patch.object(m.os, "set_blocking", side_effect=OSError("synthetic setup refusal")):
            with self.assertRaises(OSError): runner([m.HDIUTIL, "info", "-plist"])
        self.assertEqual(child.terminated, 1)
        self.assertTrue(child.stdout.closed and child.stderr.closed)

    def test_runner_caps_both_streams_and_cleans_exact_child(self):
        for index, (output, errors) in enumerate(((b"x" * 1025, b""), (b"", b"x" * 1025))):
            root = self.work / str(index); root.mkdir()
            child = self.fake_child(output, errors)
            def start(args, **kwargs):
                self.assertFalse(kwargs["shell"])
                self.assertEqual(kwargs["env"], {})
                self.assertEqual(kwargs["stdin"], m.subprocess.DEVNULL)
                return child
            runner = m.Runner(root, popen=start)
            with self.assertRaises(m.QualificationError): runner([m.HDIUTIL, "info", "-plist"], limit=1024)
            self.assertEqual(child.terminated, 1)
            self.assertEqual(child.killed, 0)
            self.assertTrue(child.stdout.closed and child.stderr.closed)
            self.assertTrue(all(path.stat().st_size <= 1024 for path in root.iterdir()))

    def test_unresolved_direct_child_is_bounded_and_retained(self):
        child = self.fake_child(b"x" * 1025)
        child.wait = mock.Mock(side_effect=m.subprocess.TimeoutExpired("synthetic", 2))
        runner = m.Runner(self.work, popen=lambda *_args, **_kwargs: child)
        with self.assertRaises(m.QualificationError): runner([m.HDIUTIL, "info", "-plist"], limit=1024)
        self.assertTrue(runner.cleanup_unresolved)
        self.assertEqual((child.terminated, child.killed), (1, 1))
        self.assertEqual(child.wait.call_args_list, [mock.call(timeout=2), mock.call(timeout=2)])
        self.assertTrue(child.stdout.closed and child.stderr.closed)

    def test_runner_rejects_late_wait_and_accepts_exact_output_cap(self):
        now = [0]
        child = self.fake_child(b"safe", on_wait=lambda: now.__setitem__(0, 2))
        runner = m.Runner(self.work, popen=lambda *_args, **_kwargs: child, clock=lambda: now[0])
        with self.assertRaises(m.QualificationError): runner([m.HDIUTIL, "info", "-plist"], seconds=1)
        root = self.work / "exact"; root.mkdir()
        child = self.fake_child(b"x" * 1024)
        runner = m.Runner(root, popen=lambda *_args, **_kwargs: child)
        self.assertEqual(runner([m.HDIUTIL, "info", "-plist"], limit=1024), b"x" * 1024)

    def test_workflow_exact_manual_arm_lane_and_explicit_optional_publication(self):
        text = (Path(__file__).resolve().parents[1] / ".github/workflows/macos-engineering-qualification.yml").read_text()
        for required in ("workflow_dispatch:", "runs-on: macos-26", "contents: read", "persist-credentials: false",
                         "github.sha == inputs.expected_source_commit", "github.run_attempt == 1", "architecture: arm64",
                         "npm@12.2.1", "1.99.1-aarch64-apple-darwin", "python3 -I -B scripts/macos-engineering-qualification.py"):
            self.assertIn(required, text)
        for required in ("publish_engineering_candidate:", "default: false", "--publication-handoff",
                         "github.ref == 'refs/heads/main'", "retention-days: 7", "digest-mismatch: error"):
            self.assertIn(required, text)
        for forbidden in ("push:", "pull_request:", "secrets.", "macos-26-xlarge"):
            self.assertNotIn(forbidden, text)


if __name__ == "__main__":
    unittest.main()
