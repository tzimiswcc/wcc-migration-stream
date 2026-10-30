#!/usr/bin/env python3
"""Synthetic files only: no hdiutil, product process, mount, app or registration."""
import contextlib
from dataclasses import replace
import importlib.util
import io
import json
import os
from pathlib import Path
import plistlib
import stat
import struct
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("macos_desktop_package", Path(__file__).with_name("package-macos-engineering-desktop.py"))
module = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(module)
a = module.assembler


class DesktopPackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="migration-stream-desktop-synthetic-")
        self.root = Path(self.temp.name).resolve()
        self.runtime = self.root / "fake runtime"
        (self.runtime / "libexec").mkdir(parents=True)
        (self.runtime / "libexec/migration-stream-local-runtime").write_bytes(b"FAKE controller never execute")
        (self.runtime / "libexec/migration-stream-local-runtime").chmod(0o555)
        (self.runtime / "data").write_bytes(b"synthetic governed bytes")
        (self.runtime / "data").chmod(0o444)
        (self.runtime / "libexec").chmod(0o555)
        self.runtime.chmod(0o555)
        self.wrapper = self.root / "fake wrapper"
        # Pure bounded Mach-O header fixture, not a runnable application.
        self.wrapper.write_bytes(struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 2, 1, 24, 0, 0)
                                 + struct.pack("<6I", 0x32, 24, 1, 26 << 16, 26 << 16, 0))
        self.wrapper.chmod(0o555)
        self.output = self.root / "new package Ω"
        self.request = a.Request(self.wrapper, self.runtime, self.output,
                                 "test.synthetic.migration-stream", "26.0", "1", "0.1.0", a.POLICY)
        self.verifications, self.commands = [], []
        self.process_guard = mock.patch.object(module.subprocess, "Popen", side_effect=AssertionError("synthetic tests never execute a process"))
        self.process_guard.start()

    def tearDown(self):
        self.process_guard.stop()
        for path, _, _ in os.walk(self.root, followlinks=False):
            os.chmod(path, 0o700)
        self.temp.cleanup()

    def report(self, root):
        self.verifications.append(root)
        return {"schemaVersion": a.CLI_SCHEMA, "ok": True, "result": {
            "bundleRoot": str(root), "manifestSha256": "a" * 64,
            "productCommit": "b" * 40, "productVersion": "0.1.0", "sourceState": "CLEAN",
            "sourceSnapshotSha256": "c" * 64,
            "target": {"os": "macos", "architecture": "arm64", "targetTriple": "aarch64-apple-darwin", "minimumOsVersion": "26.0"},
            "launcherSha256": "d" * 64, "runtimeControllerSha256": "e" * 64,
            "integrityCliSha256": "f" * 64, "workbenchSha256": "1" * 64,
            "memberCount": 2, "totalMemberBytes": 64}}

    def runner(self, command):
        self.commands.append(command)
        if command[0] == "create":
            Path(command[-1]).write_bytes(b"FAKE image bytes, no disk image generated")
        elif command[0] != "verify":
            raise AssertionError("only the fixed create/verify command pair is admitted")

    def imager(self, source, destination):
        return module._create_image(source, destination, runner=self.runner)

    @staticmethod
    def publish(source, destination):
        # Injected flow only, not a native atomic-publication qualification.
        if destination.exists() or destination.is_symlink():
            raise FileExistsError(destination)
        os.rename(source, destination)

    def package(self, request=None, **overrides):
        return module.package(request or self.request, validator=overrides.get("validator", self.report),
                              image_maker=overrides.get("image_maker", self.imager),
                              publisher=overrides.get("publisher", self.publish))

    def test_real_shared_layout_flow_bytes_modes_plist_receipt_and_fixed_commands(self):
        source = a._contents(a._snapshot(self.runtime))
        result = self.package()
        self.assertTrue(result["ok"])
        self.assertEqual(set(p.name for p in self.output.iterdir()), {module.APP_NAME, module.IMAGE_NAME, module.RECEIPT_NAME})
        app = self.output / module.APP_NAME
        self.assertEqual(a._contents(a._snapshot(app / "Contents/Resources/Runtime")), source)
        self.assertEqual((app / "Contents/MacOS/MigrationStream").read_bytes(), self.wrapper.read_bytes())
        self.assertEqual(stat.S_IMODE((app / "Contents/Resources/Runtime/data").stat().st_mode), 0o444)
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        self.assertEqual(info["CFBundleExecutable"], "MigrationStream")
        self.assertNotIn("CFBundleDocumentTypes", info)
        self.assertEqual(len(self.verifications), 4)
        receipt = json.loads((self.output / module.RECEIPT_NAME).read_bytes())
        self.assertFalse(receipt["signingPerformed"])
        self.assertFalse(receipt["mountedContentsQualified"])
        self.assertEqual(receipt["image"]["sha256"], module._image_record(self.output / module.IMAGE_NAME)["sha256"])
        self.assertEqual(self.commands[0][0:10], ["create", "-quiet", "-fs", "HFS+", "-format", "UDZO", "-volname", "Migration Stream", "-srcfolder", str(self.verifications[1].parents[3])])
        self.assertEqual(self.commands[1][0:2], ["verify", "-quiet"])
        self.assertEqual([p for p in self.root.glob('.migration-stream-desktop-*')], [])

    def test_required_metadata_and_no_public_bypasses(self):
        argv = ["--wrapper", str(self.wrapper), "--runtime", str(self.runtime), "--output-directory", str(self.output),
                "--bundle-identifier", "test.synthetic.migration-stream", "--minimum-os", "26.0", "--bundle-version", "1", "--short-version", "0.1.0", "--engineering-policy", a.POLICY]
        self.assertEqual(module.arguments(argv), self.request)
        for index in range(0, len(argv), 2):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                module.arguments(argv[:index] + argv[index + 2:])
        for flag in ("--skip-verify", "--signing-identity", "--universal", "--hdiutil"):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                module.arguments(argv + [flag, "unused"])

    def test_existing_output_preserves_foreign_sentinel_before_any_validation(self):
        self.output.mkdir()
        sentinel = self.output / "sentinel"
        sentinel.write_bytes(b"preserved")
        with self.assertRaises(a.AssemblyError): self.package()
        self.assertEqual(sentinel.read_bytes(), b"preserved")
        self.assertEqual(self.verifications, [])
        self.assertEqual(self.commands, [])

    def test_overlap_link_and_hardlink_input_refuse(self):
        for output in (self.runtime / "inside", self.wrapper, self.root):
            with self.assertRaises((a.AssemblyError, module.PackagingError)):
                self.package(replace(self.request, destination=output))
        alias = self.root / "wrapper alias"
        alias.symlink_to(self.wrapper)
        with self.assertRaises(a.AssemblyError): self.package(replace(self.request, wrapper=alias))
        alias.unlink()
        os.link(self.wrapper, alias)
        with self.assertRaises(a.AssemblyError): self.package()
        self.assertEqual(self.commands, [])

    def test_minimum_and_intel_wrapper_refuse_before_imaging(self):
        with self.assertRaises(a.AssemblyError): self.package(replace(self.request, minimum_os="25.0"))
        self.wrapper.chmod(0o755)
        data = bytearray(self.wrapper.read_bytes());struct.pack_into("<I", data, 4, 0x01000007)
        self.wrapper.write_bytes(data)
        with self.assertRaises(a.AssemblyError): self.package()
        self.assertEqual(self.commands, [])

    def test_dirty_runtime_refuses_before_image(self):
        def dirty(root):
            value = self.report(root);value["result"]["sourceState"] = "EXPLICIT_UNCOMMITTED_SNAPSHOT";return value
        with self.assertRaises(module.PackagingError): self.package(validator=dirty)
        self.assertFalse(self.output.exists())
        self.assertEqual(self.commands, [])

    def test_image_verify_failure_retains_only_private_partial(self):
        def imager(source, image):
            def fail(command):
                self.runner(command)
                if command[0] == "verify": raise module.PackagingError("synthetic checksum failure")
            return module._create_image(source, image, runner=fail)
        with self.assertRaises(module.PackagingError): self.package(image_maker=imager)
        self.assertFalse(self.output.exists())
        self.assertEqual(len(list(self.root.glob('.migration-stream-desktop-*'))), 1)

    def test_image_tamper_during_checksum_refuses(self):
        def imager(source, image):
            def tamper(command):
                self.runner(command)
                if command[0] == "verify": image.write_bytes(b"changed")
            return module._create_image(source, image, runner=tamper)
        with self.assertRaises(module.PackagingError): self.package(image_maker=imager)
        self.assertFalse(self.output.exists())

    def test_imaging_must_not_mutate_application(self):
        def imager(source, image):
            value = self.imager(source, image)
            data = source / module.APP_NAME / "Contents/Resources/Runtime/data"
            data.chmod(0o644);data.write_bytes(b"mutated copied member")
            return value
        with self.assertRaises(module.PackagingError): self.package(image_maker=imager)
        self.assertFalse(self.output.exists())
        self.assertEqual((self.runtime / "data").read_bytes(), b"synthetic governed bytes")

    def test_source_swap_during_imaging_refuses(self):
        def imager(source, image):
            value = self.imager(source, image)
            self.wrapper.chmod(0o755)
            data = self.wrapper.read_bytes();self.wrapper.write_bytes(data + b"changed")
            return value
        with self.assertRaises(module.PackagingError): self.package(image_maker=imager)
        self.assertFalse(self.output.exists())

    def test_image_links_and_small_bound_refuse_without_mutating_target(self):
        sentinel = self.root / "image sentinel";sentinel.write_bytes(b"retained");sentinel.chmod(0o644)
        def imager(_source, image):
            image.symlink_to(sentinel)
            return {"format":"UDZO", "filesystem":"HFS+", "checksumVerification":"hdiutil-exit-zero"}
        with self.assertRaises(a.AssemblyError): self.package(image_maker=imager)
        self.assertEqual(stat.S_IMODE(sentinel.stat().st_mode), 0o644)
        with mock.patch.object(module, "MAX_IMAGE", 2), self.assertRaises(module.PackagingError):
            module._image_record(sentinel)
        def hardlink(_source, image):
            os.link(sentinel, image)
            return {"format":"UDZO", "filesystem":"HFS+", "checksumVerification":"hdiutil-exit-zero"}
        with self.assertRaises(module.PackagingError): self.package(image_maker=hardlink)
        self.assertEqual(stat.S_IMODE(sentinel.stat().st_mode), 0o644)

    def test_unexpected_outer_entry_and_fixed_error_output(self):
        def imager(source, image):
            result = self.imager(source, image)
            (source.parent / "undeclared").write_bytes(b"unexpected synthetic entry")
            return result
        with self.assertRaises(module.PackagingError): self.package(image_maker=imager)
        self.assertFalse(self.output.exists())
        output = io.StringIO()
        with mock.patch.object(module, "arguments", return_value=self.request), mock.patch.object(module, "package", side_effect=OSError("private path must never print")), contextlib.redirect_stdout(output):
            self.assertEqual(module.main(), 1)
        self.assertEqual(json.loads(output.getvalue()), {"schemaVersion":module.SCHEMA,"ok":False,"code":"PACKAGING_REFUSED"})

    def exercise_fake_image_child(self, *, exit_code=0, overflow=False, expired=False):
        class Stream:
            def __init__(self, descriptor): self.descriptor, self.closed = descriptor, False
            def fileno(self): return self.descriptor
            def close(self): self.closed = True
        child = SimpleNamespace(stdout=Stream(501), stderr=Stream(502), returncode=None, terminated=False)
        child.poll = lambda: child.returncode
        def wait(timeout):
            child.returncode = 0 if child.terminated else exit_code
            return child.returncode
        def terminate(): child.terminated = True
        child.wait, child.terminate, child.kill = wait, terminate, terminate
        class Selector:
            def __init__(self): self.entries = {}
            def register(self, stream, _events): self.entries[stream.fileno()] = stream
            def get_map(self): return self.entries
            def select(self, _timeout): return [(SimpleNamespace(fd=fd, fileobj=stream), None) for fd, stream in list(self.entries.items())]
            def unregister(self, stream): del self.entries[stream.fileno()]
            def close(self): self.entries.clear()
        clock_calls = 0
        def clock():
            nonlocal clock_calls
            clock_calls += 1
            return 301 if expired and clock_calls > 1 else 0
        def read(fd, _length): return b"x" * 4096 if overflow and fd == 501 else b""
        with mock.patch.object(module.subprocess, "Popen", return_value=child) as spawn, mock.patch.object(module.selectors, "DefaultSelector", Selector), mock.patch.object(module.os, "set_blocking"), mock.patch.object(module.os, "read", side_effect=read), mock.patch.object(module.time, "monotonic", side_effect=clock):
            try:
                module._run_image_tool(["verify", "-quiet", "synthetic-image"])
            finally:
                spawn.assert_called_once_with(["/usr/bin/hdiutil", "verify", "-quiet", "synthetic-image"], env={}, shell=False, stdin=module.subprocess.DEVNULL, stdout=module.subprocess.PIPE, stderr=module.subprocess.PIPE)
                self.assertTrue(child.stdout.closed and child.stderr.closed)
                if overflow or expired: self.assertTrue(child.terminated)

    def test_image_runner_fixed_invocation_and_nonzero_refusal(self):
        self.exercise_fake_image_child()
        with self.assertRaises(module.PackagingError): self.exercise_fake_image_child(exit_code=17)

    def test_image_runner_output_cap_and_deadline_cleanup(self):
        with self.assertRaises(module.PackagingError): self.exercise_fake_image_child(overflow=True)
        with self.assertRaises(module.PackagingError): self.exercise_fake_image_child(expired=True)

    def test_final_publication_collision_preserves_foreign_output(self):
        calls = 0
        def publisher(source, destination):
            nonlocal calls
            calls += 1
            if calls == 2:
                destination.mkdir();(destination / "sentinel").write_bytes(b"foreign")
            self.publish(source, destination)
        with self.assertRaises(FileExistsError): self.package(publisher=publisher)
        self.assertEqual((self.output / "sentinel").read_bytes(), b"foreign")

    def test_post_publication_failure_preserves_visible_package(self):
        def validator(root):
            if self.output in root.parents: raise module.PackagingError("synthetic final refusal")
            return self.report(root)
        with self.assertRaises(module.PackagingError): self.package(validator=validator)
        self.assertTrue((self.output / module.APP_NAME).is_dir())
        self.assertTrue((self.output / module.IMAGE_NAME).is_file())
        self.assertTrue((self.output / module.RECEIPT_NAME).is_file())


if __name__ == "__main__":
    unittest.main()
