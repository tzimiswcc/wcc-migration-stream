#!/usr/bin/env python3
"""Fake-artifact callable tests. Never build an app or execute product bytes/verify."""
from __future__ import annotations

import contextlib
import dataclasses
import errno
import importlib.util
import io
import json
import os
import plistlib
import stat
import struct
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("engineering_assembler", Path(__file__).with_name("assemble-macos-engineering-app.py"))
assert SPEC and SPEC.loader
assembler = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = assembler
SPEC.loader.exec_module(assembler)


class EngineeringAssemblyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="migration-stream-layout-self-test-")
        self.root = Path(self.temporary.name).resolve()  # Canonicalize only our own test root.
        self.runtime = self.root / "synthetic runtime"
        (self.runtime / "libexec").mkdir(parents=True)
        (self.runtime / "libexec/migration-stream-local-runtime").write_bytes(b"FAKE controller: never execute")
        (self.runtime / "libexec/migration-stream-local-runtime").chmod(0o555)
        (self.runtime / "synthetic-data.txt").write_bytes(b"synthetic governed bytes\n")
        (self.runtime / "synthetic-data.txt").chmod(0o444)
        self.runtime.chmod(0o555)
        (self.runtime / "libexec").chmod(0o555)
        self.wrapper = self.root / "fake-wrapper"
        self.wrapper.write_bytes(b"FAKE wrapper: never execute")
        self.wrapper.chmod(0o555)
        self.destination = self.root / "ordinary-layout-directory"  # Deliberately NOT .app.
        self.request = assembler.Request(self.wrapper, self.runtime, self.destination,
                                         "test.synthetic.migration-stream", "27.0", "1", "0.2.0", assembler.POLICY)
        self.calls = []
        self.original_popen = assembler.subprocess.Popen
        def refuse_process(*_args, **_kwargs):
            raise AssertionError("fake-artifact tests must never execute a verifier/product process")
        assembler.subprocess.Popen = refuse_process
        self.inspector = lambda _path, _identity: assembler.WrapperInfo("arm64", (27, 0, 0))

    def tearDown(self):
        assembler.subprocess.Popen = self.original_popen
        for directory, _, _ in os.walk(self.root):
            os.chmod(directory, 0o700)
        self.temporary.cleanup()

    def report(self, root):
        self.calls.append(root)
        return {"schemaVersion": assembler.CLI_SCHEMA, "ok": True, "result": {
            "bundleRoot": str(root), "manifestSha256": "a" * 64,
            "productCommit": "b" * 40, "productVersion": "0.1.0", "sourceState": "CLEAN",
            "sourceSnapshotSha256": "c" * 64,
            "target": {"os": "macos", "architecture": "arm64", "targetTriple": "aarch64-apple-darwin", "minimumOsVersion": "11.0"},
            "launcherSha256": "d" * 64, "runtimeControllerSha256": "e" * 64,
            "integrityCliSha256": "f" * 64, "workbenchSha256": "1" * 64,
            "memberCount": 2, "totalMemberBytes": 64}}

    def publisher(self, source, destination):
        if sys.platform == "darwin":
            assembler._publish_noreplace(source, destination)
        else:
            # Injected flow seam only. This is NOT an atomic-publication proof.
            if destination.exists():
                raise FileExistsError(destination)
            os.rename(source, destination)

    def assemble(self, request=None, **overrides):
        return assembler.assemble(request or self.request, validator=overrides.get("validator", self.report),
                                  wrapper_inspector=overrides.get("wrapper_inspector", self.inspector),
                                  publisher=overrides.get("publisher", self.publisher))

    def assert_no_stage(self):
        self.assertEqual(list(self.root.glob(".migration-stream-engineering-*")), [])

    def test_layout_bytes_modes_plist_and_real_report_order(self):
        original = assembler._contents(assembler._snapshot(self.runtime))
        self.assertEqual(self.assemble(), self.destination)
        staged = self.destination / "Contents/Resources/Runtime"
        self.assertEqual(self.calls[0], self.runtime)
        self.assertEqual(len(self.calls), 2)
        self.assertTrue(self.calls[1].relative_to(self.root).parts[0].startswith(".migration-stream-engineering-"))
        self.assertEqual(self.calls[1].parts[-3:], ("Contents", "Resources", "Runtime"))
        self.assertEqual(assembler._contents(assembler._snapshot(staged)), original)
        wrapper = self.destination / "Contents/MacOS/MigrationStream"
        self.assertEqual(wrapper.read_bytes(), self.wrapper.read_bytes())
        self.assertEqual(stat.S_IMODE(wrapper.stat().st_mode), 0o555)
        info = plistlib.loads((self.destination / "Contents/Info.plist").read_bytes())
        self.assertEqual(info["CFBundleIdentifier"], "test.synthetic.migration-stream")
        self.assertEqual(info["LSMinimumSystemVersion"], "27.0")
        self.assertEqual(set(info), {"CFBundleExecutable", "CFBundleIdentifier", "CFBundleName", "CFBundlePackageType", "CFBundleVersion", "CFBundleShortVersionString", "LSMinimumSystemVersion"})
        self.assert_no_stage()

    def test_plist_serializer_escapes_synthetic_values_without_new_metadata_fields(self):
        path = self.root / "ordinary-plist-data"
        values = {"CFBundleName": "Synthetic <object> & 'quoted' \"name\""}
        assembler._write_plist(path, values)
        self.assertEqual(plistlib.loads(path.read_bytes()), values)
        self.assertIn(b"&amp;", path.read_bytes())
        self.assertIn(b"&lt;", path.read_bytes())

    def test_required_cli_metadata_and_no_public_skip_option(self):
        arguments = ["--wrapper", str(self.wrapper), "--runtime", str(self.runtime), "--destination", str(self.destination),
                     "--bundle-identifier", "test.synthetic.migration-stream", "--minimum-os", "27.0",
                     "--bundle-version", "1", "--short-version", "0.2.0", "--engineering-policy", assembler.POLICY]
        self.assertEqual(assembler.arguments(arguments), self.request)
        for index in range(0, len(arguments), 2):
            with self.subTest(missing=arguments[index]), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                assembler.arguments(arguments[:index] + arguments[index + 2:])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            assembler.arguments(arguments + ["--skip-verify"])

    def test_minimum_floor_and_explicit_engineering_policy(self):
        for request in [dataclasses.replace(self.request, minimum_os="26.0"), dataclasses.replace(self.request, engineering_policy="signed-release"), dataclasses.replace(self.request, bundle_identifier=""), dataclasses.replace(self.request, bundle_version=""), dataclasses.replace(self.request, short_version="")]:
            with self.subTest(request=request), self.assertRaises(assembler.AssemblyError):
                self.assemble(request)
            self.assertFalse(self.destination.exists())
            self.assert_no_stage()
        def higher_runtime(root):
            result = self.report(root)
            result["result"]["target"]["minimumOsVersion"] = "28.0"
            return result
        with self.assertRaises(assembler.AssemblyError):
            self.assemble(validator=higher_runtime)
        self.assert_no_stage()

    def test_existing_destination_and_overlap_never_replace(self):
        self.destination.mkdir()
        sentinel = self.destination / "sentinel"
        sentinel.write_bytes(b"retained")
        with self.assertRaises(assembler.AssemblyError):
            self.assemble()
        self.assertEqual(sentinel.read_bytes(), b"retained")
        self.assertEqual(self.calls, [])
        for output in (self.runtime / "inside", self.wrapper, self.root):
            with self.assertRaises(assembler.AssemblyError):
                self.assemble(dataclasses.replace(self.request, destination=output))
        self.assert_no_stage()

    def test_symlink_hardlink_special_and_linked_parent_refusals(self):
        link = self.root / "wrapper-link"
        link.symlink_to(self.wrapper)
        with self.assertRaises(assembler.AssemblyError):
            self.assemble(dataclasses.replace(self.request, wrapper=link))
        parent_link = self.root / "parent-link"
        parent_link.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(assembler.AssemblyError):
            self.assemble(dataclasses.replace(self.request, destination=parent_link / "output"))
        os.link(self.wrapper, self.root / "alias")
        with self.assertRaises(assembler.AssemblyError):
            self.assemble()
        (self.root / "alias").unlink()
        fifo = self.root / "fifo"
        os.mkfifo(fifo)
        with self.assertRaises(assembler.AssemblyError):
            self.assemble(dataclasses.replace(self.request, wrapper=fifo))
        self.assertEqual(self.calls, [])
        self.assert_no_stage()

    def test_verifier_failure_order_cleanup_identity_drift_and_no_admission_copy(self):
        for stage_failure in (False, True):
            self.calls.clear()
            def fail(root):
                result = self.report(root)
                if stage_failure and root == self.runtime:
                    return result
                raise assembler.AssemblyError("synthetic verify refusal")
            with self.assertRaises(assembler.AssemblyError):
                self.assemble(validator=fail)
            self.assertEqual(len(self.calls), 2 if stage_failure else 1)
            self.assertFalse(self.destination.exists())
            self.assert_no_stage()
        def drift(root):
            result = self.report(root)
            if root != self.runtime:
                result["result"]["manifestSha256"] = "2" * 64
            return result
        with self.assertRaises(assembler.AssemblyError):
            self.assemble(validator=drift)
        def invented(root):
            result = self.report(root)
            result["result"]["admission"] = "invented"
            return result
        with self.assertRaises(assembler.AssemblyError):
            self.assemble(validator=invented)
        self.assert_no_stage()

    def test_staged_byte_or_mode_corruption_refuses_and_cleans_only_own_stage(self):
        sibling = self.root / "unrelated-sibling"
        sibling.mkdir()
        (sibling / "retained").write_bytes(b"untouched")
        for mutation in ("bytes", "mode"):
            def corrupt(root):
                result = self.report(root)
                if root != self.runtime:
                    data = root / "synthetic-data.txt"
                    data.chmod(0o600)
                    if mutation == "bytes":
                        data.write_bytes(b"changed synthetic bytes")
                return result
            with self.subTest(mutation=mutation), self.assertRaises(assembler.AssemblyError):
                self.assemble(validator=corrupt)
            self.assertEqual((sibling / "retained").read_bytes(), b"untouched")
            self.assertFalse(self.destination.exists())
            self.assert_no_stage()

    @unittest.skipUnless(sys.platform == "darwin", "Darwin atomic no-replace primitive is not exercised on this host")
    def test_native_atomic_no_replace_destination_race(self):
        retained_identity = []
        def race(source, destination):
            destination.mkdir()  # Empty: a plain rename would silently replace it.
            info = destination.stat()
            retained_identity.append((info.st_dev, info.st_ino))
            assembler._publish_noreplace(source, destination)
        with self.assertRaises(OSError) as error:
            self.assemble(publisher=race)
        self.assertEqual(error.exception.errno, errno.EEXIST)
        current = self.destination.stat()
        self.assertEqual((current.st_dev, current.st_ino), retained_identity[0])
        self.assertEqual(list(self.destination.iterdir()), [])
        self.assert_no_stage()

    def test_post_publication_error_never_deletes_visible_output(self):
        def publication_error(source, destination):
            self.publisher(source, destination)
            raise assembler.AssemblyError("synthetic error after namespace publication")
        with self.assertRaises(assembler.AssemblyError):
            self.assemble(publisher=publication_error)
        self.assertTrue((self.destination / "Contents/Info.plist").is_file())
        self.assert_no_stage()

    def test_snapshot_streaming_global_entry_and_depth_bounds_before_verify(self):
        # A synthetic directory iterator cannot be eagerly exhausted or descended.
        graph = self.root / "oversized-graph"
        graph.mkdir()
        for name in ("a", "b"):
            (graph / name).mkdir()
        consumed = []
        closed = []
        original_scandir = os.scandir
        def synthetic_entries(path):
            for index in range(10000):
                consumed.append(index)
                if len(consumed) > 4094:
                    raise AssertionError("enumeration consumed beyond the global budget")
                yield type("Entry", (), {"path": str(path / f"unvisited-{index}")})()
        @contextlib.contextmanager
        def bounded_fixture(path):
            if path == graph / "a":
                iterator = synthetic_entries(path)
                try:
                    yield iterator
                finally:
                    iterator.close()
                    closed.append(True)
            else:
                with original_scandir(path) as iterator:
                    yield iterator
        with mock.patch.object(assembler.os, "scandir", bounded_fixture):
            with self.assertRaisesRegex(assembler.AssemblyError, "graph exceeds"):
                self.assemble(dataclasses.replace(self.request, runtime=graph))
        # Root + a + b reserve 3 entries; 4093 names fit, next refuses immediately.
        self.assertEqual(len(consumed), 4094)
        self.assertEqual(closed, [True])
        self.assertEqual(sorted(child.name for child in graph.iterdir()), ["a", "b"])
        self.assertEqual(list((graph / "a").iterdir()), [])
        self.assertEqual(self.calls, [])
        self.assertFalse(self.destination.exists())
        self.assert_no_stage()

        deep = self.root / "deep-graph"
        deep.mkdir()
        leaf = deep
        for _ in range(65):
            leaf = leaf / "d"
            leaf.mkdir()
        with self.assertRaisesRegex(assembler.AssemblyError, "depth exceeds"):
            self.assemble(dataclasses.replace(self.request, runtime=deep))
        self.assertTrue(leaf.is_dir())
        self.assertEqual(list(leaf.iterdir()), [])
        self.assertEqual(self.calls, [])
        self.assertFalse(self.destination.exists())
        self.assert_no_stage()

    def test_bounded_thin_macho_deployment_parser(self):
        path = self.root / "fake-MachO-data"
        command = struct.pack("<6I", 0x32, 24, 1, 27 << 16, 27 << 16, 0)
        def artifact(commands=command, count=1, cpu=0x0100000C, magic=0xFEEDFACF, kind=2):
            return struct.pack("<8I", magic, cpu, 0, kind, count, len(commands), 0, 0) + commands
        path.write_bytes(artifact())
        self.assertEqual(assembler._inspect_wrapper(path), assembler.WrapperInfo("arm64", (27, 0, 0)))
        for data in [b"FAKE not MachO", artifact(cpu=7), artifact(magic=0xCAFEBABE), artifact(kind=6), artifact(count=0), artifact(count=4097), artifact(commands=command * 2, count=2), artifact(commands=struct.pack("<2I", 1, 0)), artifact(commands=struct.pack("<2I", 1, 8)), artifact(commands=command[:-1]), artifact(commands=struct.pack("<6I", 0x32, 24, 6, 27 << 16, 0, 0))]:
            path.write_bytes(data)
            with self.subTest(data=data[:32]), self.assertRaises(assembler.AssemblyError):
                assembler._inspect_wrapper(path)


if __name__ == "__main__":
    print("Fake ordinary-layout fixtures only: no .app/product bytes, verifier process, UI, runtime, signing or OS registration.", flush=True)
    unittest.main()
