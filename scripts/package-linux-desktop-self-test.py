#!/usr/bin/env python3
"""Pure/synthetic packaging proof; no native product/tools/desktop operations."""
import contextlib
from dataclasses import replace
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


p = load("linux_desktop_packaging_test_subject", ROOT / "scripts/package-linux-desktop.py")
desktop = load("linux_desktop_composition_test_subject", ROOT / "native/linux/packaged_launcher.py")


def elf(machine):
    data = bytearray(b"\x7fELF\x02\x01\x01" + bytes(57))
    data[18:20] = machine.to_bytes(2, "little")
    return bytes(data)


def ar_members(data):
    assert data[:8] == b"!<arch>\n"
    members, offset = {}, 8
    while offset < len(data):
        header = data[offset:offset + 60]
        assert len(header) == 60 and header[58:] == b"`\n"
        name = header[:16].decode("ascii").strip().removesuffix("/")
        assert name not in members
        size = int(header[48:58])
        offset += 60
        members[name] = data[offset:offset + size]
        assert len(members[name]) == size
        offset += size + size % 2
    assert offset == len(data)
    return members


def tar_members(data):
    result = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive:
            assert member.name not in result and (member.isdir() or member.isfile())
            assert member.uid == member.gid == 0
            result[member.name] = (member.mode, None if member.isdir() else archive.extractfile(member).read())
    return result


def raw_tar_headers(data, *, partial=False):
    """Check actual 512-byte headers, without tarfile's transparent PAX reader."""
    decoded = gzip.decompress(data)
    headers, offset = [], 0
    while offset < len(decoded):
        header = decoded[offset:offset + 512]
        assert len(header) == 512
        if header == bytes(512):
            assert decoded[offset:] == bytes(len(decoded) - offset)
            assert partial or len(decoded) - offset >= 1024
            return headers
        assert header[257:263] == b"ustar\0" and header[263:265] == b"00"
        assert header[156:157] in (b"0", b"5")  # No PAX x/g or link/special types.
        checksum = int(header[148:156].strip(b" \0"), 8)
        assert checksum == sum(header[:148] + b" " * 8 + header[156:])
        size = int(header[124:136].strip(b" \0"), 8)
        headers.append(header)
        offset += 512 + ((size + 511) // 512) * 512
    assert partial and offset == len(decoded)
    return headers


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="linux-package-synthetic-")
        self.root = Path(self.temporary.name).resolve()
        self.runtime = self.root / "runtime"
        self.runtime.mkdir()
        self.files = {"migration-stream-local": b"#!/bin/sh\n# synthetic, never executed\n",
                      **{name: elf(62) for name in p.ELF_PATHS}, "bundle-manifest.json": b"{}\n"}
        for name, data in self.files.items():
            path = self.runtime / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(0o444 if name.endswith(".json") else 0o555)
        for directory in sorted([self.runtime, *[v for v in self.runtime.rglob("*") if v.is_dir()]], reverse=True):
            directory.chmod(0o555)
        self.request = p.Request(self.runtime, self.root / "output", "x64", "1" * 40,
                                 "2" * 64, "3" * 64, "Synthetic Maintainer <fixture@example.invalid>", 1, p.POLICY)
        self.calls = []

    def tearDown(self):
        # Only this test's disposable owned synthetic root is unsealed/removed.
        for directory, _, _ in os.walk(self.root):
            Path(directory).chmod(0o700)
        self.temporary.cleanup()

    def verify(self, root):
        self.calls.append(root)
        rows = p._snapshot(root)
        result = {"bundleRoot": str(root), "manifestSha256": "3" * 64,
                  "productCommit": "1" * 40, "productVersion": "0.1.0", "sourceState": "CLEAN",
                  "sourceSnapshotSha256": "2" * 64,
                  "target": {"os": "linux", "architecture": self.request.architecture,
                             "targetTriple": p.TARGETS[self.request.architecture][1]},
                  "memberCount": 4, "totalMemberBytes": sum(len(v) for v in self.files.values()) - 3}
        for key, name in (("launcherSha256", "migration-stream-local"),
                          ("runtimeControllerSha256", p.ELF_PATHS[0]),
                          ("integrityCliSha256", p.ELF_PATHS[1]), ("workbenchSha256", p.ELF_PATHS[2])):
            result[key] = rows[name][3]
        return {"schemaVersion": p.CLI_SCHEMA, "ok": True, "result": result}

    @staticmethod
    def publish(stage, destination):
        # A Mac test seam, not a replacement native publication implementation.
        if os.path.lexists(destination):
            raise p.PackagingError("synthetic collision")
        stage.rename(destination)

    def package(self, **changes):
        options = {"verify": self.verify, "dependencies": lambda *_: "libc6 (>= 2.39), libgcc-s1",
                   "native": lambda _: "24.04", "publish": self.publish}
        options.update(changes)
        return p._package(self.request, **options)

    def mutate(self, name, data):
        path = self.runtime / name
        path.chmod(0o644)
        path.write_bytes(data)
        path.chmod(0o555)

    def test_both_formats_preserve_runtime_modes_graph_bytes_and_control(self):
        original = p._snapshot(self.runtime)
        receipt = self.package()
        self.assertEqual(len(self.calls), 2)
        self.assertEqual(p._snapshot(self.runtime), original)
        self.assertEqual(set(receipt["artifacts"]), {
            "migration-stream_0.1.0+git111111111111-1_amd64.deb",
            "migration-stream-0.1.0+git111111111111-1-linux-x64-gnu.tar.gz"})
        for name, expected in receipt["artifacts"].items():
            artifact = (self.request.destination / name).read_bytes()
            self.assertEqual(hashlib.sha256(artifact).hexdigest(), expected["sha256"])
            if name.endswith(".deb"):
                ar = ar_members(artifact)
                self.assertEqual(list(ar), ["debian-binary", "control.tar.gz", "data.tar.gz"])
                self.assertEqual(ar["debian-binary"], b"2.0\n")
                self.assertTrue(raw_tar_headers(ar["control.tar.gz"]))
                self.assertTrue(raw_tar_headers(ar["data.tar.gz"]))
                control = tar_members(ar["control.tar.gz"])
                self.assertEqual(set(control), {"control"})  # No maintainer/autostart scripts.
                text = control["control"][1].decode()
                self.assertIn("Architecture: amd64\n", text)
                self.assertIn("libc6 (>= 2.39), libgcc-s1\n", text)
                data = tar_members(ar["data.tar.gz"])
                prefix = "usr/lib/migration-stream/Runtime/"
                entry = data["usr/share/applications/migration-stream.desktop"][1].decode()
                self.assertIn(" -I /usr/lib/migration-stream/packaged_launcher.py %f\n", entry)
                self.assertIn("NoDisplay=true\n", entry)
                self.assertIn("*.mstream", data["usr/share/mime/packages/migration-stream.xml"][1].decode())
            else:
                self.assertTrue(raw_tar_headers(artifact))
                data = tar_members(artifact)
                prefix = "migration-stream-0.1.0+git111111111111-1-linux-x64-gnu/Runtime/"
            captured = {name[len(prefix):]: value for name, value in data.items() if name.startswith(prefix)}
            self.assertEqual(set(captured), set(original) - {"."})
            for name, row in original.items():
                if name != ".":
                    self.assertEqual(captured[name][0], row[1])
                    self.assertEqual(captured[name][1], None if row[0] == "directory" else self.files[name])

    def test_arm64_maps_to_debian_arm64_and_requires_arm64_elf(self):
        self.request = replace(self.request, architecture="arm64")
        for name in p.ELF_PATHS:
            self.mutate(name, elf(183))
        result = self.package()
        self.assertEqual(result["architecture"], "arm64")
        self.assertIn("_arm64.deb", next(name for name in result["artifacts"] if name.endswith(".deb")))

    def test_wrong_native_machine_refuses_before_publication(self):
        self.mutate(p.ELF_PATHS[0], elf(183))
        with self.assertRaises(p.PackagingError):
            self.package()
        self.assertFalse(self.request.destination.exists())
        self.assertEqual(self.calls, [])

    def test_copied_wrong_machine_refuses_before_second_verifier(self):
        original_copy = p._copy
        def wrong_copy(source, destination, rows):
            original_copy(source, destination, rows)
            if source == self.runtime:
                controller = destination / p.ELF_PATHS[0]
                controller.chmod(0o644)
                controller.write_bytes(elf(183))
                controller.chmod(0o555)
        with patch.object(p, "_copy", side_effect=wrong_copy), self.assertRaises(p.PackagingError):
            self.package()
        self.assertEqual(self.calls, [self.runtime])
        self.assertFalse(self.request.destination.exists())

    def test_production_linux_target_omits_optional_os_floor_and_refuses_extras(self):
        rows = p._snapshot(self.runtime)
        value = self.verify(self.runtime)
        self.assertEqual(value["result"]["target"], {
            "os": "linux", "architecture": "x64", "targetTriple": "x86_64-unknown-linux-gnu"})
        result = p._report(value, self.runtime, self.request, "0.1.0", rows)
        self.assertEqual(set(result["target"]), {"os", "architecture", "targetTriple"})
        for field, replacement in (("minimumOsVersion", None), ("unexpected", "extra"),
                                   ("architecture", "arm64"), ("targetTriple", "aarch64-unknown-linux-gnu")):
            with self.subTest(field=field):
                invalid = self.verify(self.runtime)
                invalid["result"]["target"][field] = replacement
                with self.assertRaises(p.PackagingError):
                    p._report(invalid, self.runtime, self.request, "0.1.0", rows)

    def test_existing_output_and_sentinel_are_preserved_before_verifier(self):
        self.request.destination.mkdir()
        sentinel = self.request.destination / "sentinel"
        sentinel.write_bytes(b"foreign")
        with self.assertRaises(p.PackagingError):
            self.package()
        self.assertEqual(sentinel.read_bytes(), b"foreign")
        self.assertEqual(self.calls, [])

    def test_collision_at_publication_preserves_foreign_output(self):
        def collision(stage, destination):
            destination.mkdir()
            (destination / "sentinel").write_bytes(b"foreign")
            self.publish(stage, destination)
        with self.assertRaises(p.PackagingError):
            self.package(publish=collision)
        self.assertEqual((self.request.destination / "sentinel").read_bytes(), b"foreign")
        self.assertEqual(list(self.request.destination.iterdir()), [self.request.destination / "sentinel"])

    def test_source_target_commit_snapshot_and_manifest_cuts_are_strict(self):
        for key, value in (("sourceState", "EXPLICIT_UNCOMMITTED_SNAPSHOT"),
                           ("productCommit", "4" * 40), ("sourceSnapshotSha256", "4" * 64),
                           ("manifestSha256", "4" * 64), ("productVersion", "9.0.0"),
                           ("target", {"os": "linux", "architecture": "arm64"})):
            with self.subTest(key=key):
                def invalid(root):
                    report = self.verify(root)
                    report["result"][key] = value
                    return report
                with self.assertRaises(p.PackagingError):
                    self.package(verify=invalid)
                self.assertFalse(self.request.destination.exists())

    def test_verifier_refusal_leaves_no_visible_output(self):
        def refusing(root):
            if root != self.runtime:
                raise p.PackagingError("synthetic staged verifier refusal")
            return self.verify(root)
        with self.assertRaises(p.PackagingError):
            self.package(verify=refusing)
        self.assertFalse(self.request.destination.exists())

    def test_report_hash_must_bind_actual_captured_program(self):
        def wrong_hash(root):
            result = self.verify(root)
            result["result"]["runtimeControllerSha256"] = "e" * 64
            return result
        with self.assertRaises(p.PackagingError):
            self.package(verify=wrong_hash)
        self.assertFalse(self.request.destination.exists())

    def test_identical_byte_source_swap_is_refused(self):
        def swapped(root):
            result = self.verify(root)
            if root != self.runtime:
                path = self.runtime / p.ELF_PATHS[0]
                path.parent.chmod(0o755)
                path.rename(path.with_suffix(".old"))
                path.write_bytes(self.files[p.ELF_PATHS[0]])
                path.chmod(0o555)
                path.parent.chmod(0o555)
            return result
        with self.assertRaises(p.PackagingError):
            self.package(verify=swapped)
        self.assertFalse(self.request.destination.exists())

    def test_staged_mutation_during_dependency_analysis_is_refused(self):
        def dependencies(root, *_):
            path = root / p.ELF_PATHS[1]
            path.chmod(0o644)
            path.write_bytes(b"changed")
            return "libc6"
        with self.assertRaises(p.PackagingError):
            self.package(dependencies=dependencies)
        self.assertFalse(self.request.destination.exists())

    def test_hardlink_and_symlink_sources_are_refused(self):
        path = self.runtime / p.ELF_PATHS[0]
        alias = self.root / "alias"
        os.link(path, alias)
        with self.assertRaises(p.PackagingError):
            self.package()
        alias.unlink()
        self.runtime.chmod(0o755)
        (self.runtime / "linked").symlink_to(path)
        with self.assertRaises(p.PackagingError):
            self.package()
        self.assertEqual(self.calls, [])

    def test_graph_and_byte_caps_refuse_before_verifier(self):
        for field, value in (("MAX_ENTRIES", 3), ("MAX_TOTAL", 2), ("MAX_FILE", 16)):
            with self.subTest(field=field), patch.object(p, field, value):
                with self.assertRaises(p.PackagingError):
                    self.package()
        self.assertEqual(self.calls, [])

    def test_archive_stream_cap_retains_only_private_partial(self):
        with patch.object(p, "MAX_ARCHIVE", 20), self.assertRaises(p.PackagingError):
            self.package()
        self.assertFalse(self.request.destination.exists())

    def test_ustar_refuses_unrepresentable_long_name_without_pax_fallback(self):
        source = self.root / "long-name-input"
        source.mkdir()
        # One basename over USTAR's 100-byte field cannot use its prefix split.
        (source / ("n" * 101)).write_bytes(b"synthetic source member")
        output = self.root / "private-partial.tar.gz"
        with self.assertRaises(ValueError):
            p._tar(output, source, p._snapshot(source), "portable")
        headers = raw_tar_headers(output.read_bytes(), partial=True)
        self.assertEqual(len(headers), 1)  # Only the ordinary root header preceded refusal.
        self.assertEqual(headers[0][156:157], b"5")

    def test_dependency_control_injection_is_refused(self):
        with self.assertRaises(p.PackagingError):
            self.package(dependencies=lambda *_: "libc6\npostinst: bad")
        self.assertFalse(self.request.destination.exists())

    def test_native_cli_guard_and_public_verifier_override_refusal(self):
        with patch.object(p.sys, "platform", "darwin"), self.assertRaises(p.PackagingError):
            p._native("x64")
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
            p.main(["--skip-verify"])
        self.assertEqual(caught.exception.code, 2)

    def test_duplicate_json_and_boolean_counts_refuse(self):
        with self.assertRaises(p.PackagingError):
            p._json(b'{"ok":true,"\\u006fk":true}')
        def boolean(root):
            value = self.verify(root)
            value["result"]["memberCount"] = True
            return value
        with self.assertRaises(p.PackagingError):
            self.package(verify=boolean)

    def test_desktop_environment_allowlist_excludes_execution_overrides(self):
        value = desktop.presenter_environment({"DISPLAY": ":0", "HOME": "/synthetic/user",
                                               "BROWSER": "bad", "LD_PRELOAD": "bad", "PYTHONPATH": "bad"})
        self.assertEqual(value, {"PATH": "/usr/bin:/bin", "DISPLAY": ":0", "HOME": "/synthetic/user"})
        self.assertIn(" %f\n", desktop.desktop_entry(Path("/synthetic/layout/packaged_launcher.py")))
        for name in ("/synthetic/space path/run.py", "/synthetic/%f/run.py", "/synthetic/../run.py"):
            with self.assertRaises(ValueError):
                desktop.desktop_entry(Path(name))

    def test_user_registration_is_create_only_and_never_defaults(self):
        data = self.root / "data"
        data.mkdir()
        calls = []
        def run(argv, **options):
            calls.append((argv, options))
            return type("SyntheticOutput", (), {"status": 0})()
        with patch.object(desktop.os, "geteuid", return_value=data.stat().st_uid or 1000):
            if data.stat().st_uid == 0:
                self.skipTest("user registration requires non-root fixture owner")
            desktop.register_desktop(self.root, data, run)
            self.assertEqual([row[0][0] for row in calls], [b"/usr/bin/update-mime-database", b"/usr/bin/update-desktop-database"])
            self.assertFalse((data / "applications/mimeapps.list").exists())
            first = (data / "applications/migration-stream.desktop").read_bytes()
            with self.assertRaises(ValueError):
                desktop.register_desktop(self.root, data, run)
            self.assertEqual((data / "applications/migration-stream.desktop").read_bytes(), first)
            self.assertEqual(len(calls), 2)

    def test_registration_tool_failure_has_only_fixed_public_error(self):
        captured = io.StringIO()
        with patch.object(desktop, "register_desktop", side_effect=RuntimeError("synthetic private detail")), contextlib.redirect_stderr(captured):
            status = desktop.main(["--register-desktop", str(self.root)])
        self.assertEqual(status, 1)
        self.assertEqual(captured.getvalue(), "Migration Stream Linux desktop operation failed.\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
