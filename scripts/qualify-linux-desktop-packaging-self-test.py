#!/usr/bin/env python3
"""Tiny synthetic qualification parser/inspection cases; never run native tools."""
import contextlib
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


q = load("linux_qualification_test_subject", "scripts/qualify-linux-desktop-packaging.py")
p = load("linux_packaging_test_helpers", "scripts/package-linux-desktop.py")
adapter = load("linux_qualification_adapter_test_helpers", "native/linux/packaged_launcher.py")


def environment(architecture="x64"):
    return {"QUALIFICATION_ARCHITECTURE": architecture, "EXPECTED_SOURCE_COMMIT": "1" * 40,
            "GITHUB_SHA": "1" * 40, "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_RUN_ID": "123", "GITHUB_REPOSITORY": "tzimiswcc/wcc-migration-stream",
            "RUNNER_OS": "Linux", "RUNNER_ARCH": "ARM64" if architecture == "arm64" else "X64",
            "RUSTUP_TOOLCHAIN": "1.99.1-" + q.LANES[architecture][1],
            "DEBIAN_MAINTAINER": "Synthetic Maintainer <fixture@example.invalid>"}


def tar_bytes(entries, format=tarfile.USTAR_FORMAT):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=format) as archive:
        for name, kind, mode, data in entries:
            info = tarfile.TarInfo(name)
            info.uid = info.gid = info.mtime = 0
            info.mode = mode
            info.type = tarfile.DIRTYPE if kind == "directory" else tarfile.REGTYPE
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data) if data else None)
    return output.getvalue()


def elf(machine):
    data = bytearray(b"\x7fELF\x02\x01\x01" + bytes(57))
    data[18:20] = machine.to_bytes(2, "little")
    return bytes(data)


class SourceAndFormats(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="linux-qualification-synthetic-")
        self.root = Path(self.temporary.name).resolve()

    def tearDown(self):
        for root, _, _ in os.walk(self.root):
            Path(root).chmod(0o700)
        self.temporary.cleanup()

    def admission(self, env):
        return q.admission(env, "linux", q.LANES[env["QUALIFICATION_ARCHITECTURE"]][0],
                           {"ID": "ubuntu", "VERSION_ID": "24.04"})

    def test_both_exact_native_lane_tuples_are_admitted(self):
        for lane in q.LANES:
            self.assertEqual(self.admission(environment(lane)), (lane, "1" * 40))

    def test_wrong_os_machine_series_and_runner_architecture_refuse(self):
        env = environment()
        for system, machine, series in [("darwin", "x86_64", {"ID": "ubuntu", "VERSION_ID": "24.04"}),
                                         ("linux", "aarch64", {"ID": "ubuntu", "VERSION_ID": "24.04"}),
                                         ("linux", "x86_64", {"ID": "debian", "VERSION_ID": "24.04"})]:
            with self.assertRaises(q.Refused):
                q.admission(env, system, machine, series)
        env["RUNNER_ARCH"] = "ARM64"
        with self.assertRaises(q.Refused):
            self.admission(env)

    def test_source_attempt_event_repository_and_toolchain_cuts_refuse(self):
        for key, wrong in [("GITHUB_SHA", "2" * 40), ("GITHUB_RUN_ATTEMPT", "2"),
                           ("GITHUB_EVENT_NAME", "push"), ("GITHUB_REPOSITORY", "other/repo"),
                           ("RUSTUP_TOOLCHAIN", "stable")]:
            env = environment()
            env[key] = wrong
            with self.assertRaises(q.Refused):
                self.admission(env)

    def test_explicit_maintainer_and_build_overrides_are_bounded(self):
        for key, wrong in [("DEBIAN_MAINTAINER", "Name <x@y>\nDepends: bad"),
                           ("DEBIAN_MAINTAINER", ""), ("RUSTC_WRAPPER", "foreign"),
                           ("VITE_SYNTHETIC", "value")]:
            env = environment()
            env[key] = wrong
            with self.assertRaises(q.Refused):
                self.admission(env)

    def assembly(self):
        return {"schemaVersion": "migration-stream/assisted-local-runtime/assembly/v1", "ok": True,
                "result": {"bundleDirectory": "/synthetic/runtime", "manifestSha256": "3" * 64,
                           "memberCount": 4, "totalMemberBytes": 256}}

    def test_exact_final_pretty_json_suffix_is_admitted(self):
        data = b"synthetic build output\n" + json.dumps(self.assembly(), indent=2).encode() + b"\n"
        self.assertEqual(q.assembly_result(data, Path("/synthetic/runtime"), p._json), self.assembly()["result"])

    def test_assembly_suffix_extra_data_archive_keys_and_boolean_counts_refuse(self):
        original = json.dumps(self.assembly(), indent=2).encode()
        for data in [original + b"\nforeign", original + b"\n{}\n", b"prefix only", b"x" * (256 * 1024 + 1)]:
            with self.assertRaises((q.Refused, p.PackagingError)):
                q.assembly_result(data, Path("/synthetic/runtime"), p._json)
        for key, value in [("archivePath", "/synthetic/unexpected.zip"), ("memberCount", True)]:
            document = self.assembly()
            document["result"][key] = value
            with self.assertRaises(q.Refused):
                q.assembly_result(json.dumps(document).encode(), Path("/synthetic/runtime"), p._json)

    def test_manifest_source_target_and_optional_field_mix_refuse(self):
        value = {"manifestSha256": "3" * 64,
                 "product": {"name": "migration-stream", "version": "0.1.0", "commit": "1" * 40,
                             "sourceState": "CLEAN", "sourceSnapshotSha256": "2" * 64},
                 "target": {"os": "linux", "architecture": "x64", "targetTriple": q.LANES["x64"][1]}}
        self.assertEqual(q.manifest_pins(value, self.assembly()["result"], "x64", "1" * 40, "0.1.0"), "2" * 64)
        value["target"]["minimumOsVersion"] = None
        with self.assertRaises(q.Refused):
            q.manifest_pins(value, self.assembly()["result"], "x64", "1" * 40, "0.1.0")
        del value["target"]["minimumOsVersion"]
        value["product"]["commit"] = "4" * 40
        with self.assertRaises(q.Refused):
            q.manifest_pins(value, self.assembly()["result"], "x64", "1" * 40, "0.1.0")

    def test_raw_debian_order_lengths_and_version_are_checked(self):
        path = self.root / "synthetic.deb"
        p._ar(path, (("debian-binary", b"2.0\n"), ("control.tar.gz", b"abc"), ("data.tar.gz", b"def")))
        q.deb_layout(path)
        data = path.read_bytes()
        for wrong in [data[:-1], data + b"foreign", data.replace(b"2.0\n", b"3.0\n", 1),
                      data.replace(b"data.tar.gz", b"other.targz", 1)]:
            bad = self.root / "bad.deb"
            bad.write_bytes(wrong)
            with self.assertRaises(q.Refused):
                q.deb_layout(bad)

    def test_selected_control_fields_are_exact_and_duplicate_free(self):
        data = b"Package: migration-stream\nVersion: 0.1.0\nArchitecture: amd64\nMaintainer: Fixture <x@y>\nDepends: libc6\n"
        self.assertEqual(q.control_fields(data)["Architecture"], "amd64")
        for wrong in [data + b"Depends: foreign\n", data + b"unexpected\n", data.replace(b"Package: ", b"Package=", 1)]:
            with self.assertRaises(q.Refused):
                q.control_fields(wrong)

    def test_ustar_extracts_only_private_regular_bytes_and_modes(self):
        source = self.root / "synthetic.tar"
        source.write_bytes(tar_bytes([("layout", "directory", 0o755, b""),
                                      ("layout/file", "file", 0o444, b"synthetic")]))
        target = self.root / "inspection"
        q.extract_ustar(source, target)
        self.assertEqual((target / "layout/file").read_bytes(), b"synthetic")
        self.assertEqual((target / "layout/file").stat().st_mode & 0o777, 0o444)
        with self.assertRaises(FileExistsError):
            q.extract_ustar(source, target)

    def test_gzip_portable_payload_is_inspected(self):
        source = self.root / "synthetic.tar.gz"
        source.write_bytes(gzip.compress(tar_bytes([("layout", "directory", 0o755, b"")]), mtime=0))
        q.extract_ustar(source, self.root / "inspection", compressed=True)

    def test_ustar_traversal_pax_duplicates_and_mode_changes_refuse(self):
        cases = [tar_bytes([("../escape", "file", 0o444, b"x")]),
                 tar_bytes([("/absolute", "file", 0o444, b"x")]),
                 tar_bytes([("same", "file", 0o444, b"x"), ("same", "file", 0o444, b"x")]),
                 tar_bytes([("unsafe", "file", 0o4777, b"x")]),
                 tar_bytes([("n" * 101, "file", 0o444, b"x")], tarfile.PAX_FORMAT)]
        for index, data in enumerate(cases):
            source = self.root / (str(index) + ".tar")
            source.write_bytes(data)
            with self.assertRaises((q.Refused, FileExistsError)):
                q.extract_ustar(source, self.root / ("inspection" + str(index)))
        self.assertFalse((self.root / "escape").exists())

    def test_ustar_checksum_truncation_and_trailing_bytes_refuse(self):
        original = tar_bytes([("file", "file", 0o444, b"value")])
        changed = bytearray(original)
        changed[0] ^= 1
        for index, value in enumerate([bytes(changed), original[:600], original + b"foreign"]):
            path = self.root / ("bad" + str(index) + ".tar")
            path.write_bytes(value)
            with self.assertRaises(q.Refused):
                q.extract_ustar(path, self.root / ("bad-tree" + str(index)))

    def test_elf_tool_admission_reads_without_executing_and_refuses_opposite_isa(self):
        for machine in (62, 183):
            path = self.root / str(machine)
            path.write_bytes(elf(machine))
            path.chmod(0o755)
            actual, digest = q.executable(path, machine)
            self.assertEqual(actual, path)
            self.assertEqual(digest, hashlib.sha256(elf(machine)).hexdigest())
            with self.assertRaises(q.Refused):
                q.executable(path, 183 if machine == 62 else 62)

    def test_artifact_inventory_uses_archive_cap_and_refuses_extra_members(self):
        root = self.root / "output"
        root.mkdir()
        (root / "package-receipt.json").write_bytes(b"{}")
        (root / "synthetic.deb").write_bytes(b"x" * 32)
        with patch.object(p, "MAX_FILE", 8):
            q.artifact_state(p, root, ("package-receipt.json", "synthetic.deb"))
        (root / "extra").write_bytes(b"x")
        with self.assertRaises(q.Refused):
            q.artifact_state(p, root, ("package-receipt.json", "synthetic.deb"))

    def test_known_late_eof_capture_cannot_report_command_success(self):
        clock, waits = [0.0], []
        class Pipe:
            def __init__(self, descriptor):
                self.descriptor = descriptor
            def fileno(self):
                return self.descriptor
            def close(self):
                pass
        class Child:
            stdout, stderr = Pipe(1001), Pipe(1002)
            def poll(self):
                return 0
            def wait(self, *, timeout):
                waits.append(timeout)
                return 0
        class Selector:
            def __init__(self):
                self.entries = {}
            def register(self, pipe, _events, index):
                self.entries[pipe.fileno()] = type("Key", (), {"fileobj": pipe, "data": index})()
            def get_map(self):
                return self.entries
            def select(self, _timeout):
                return [(key, q.selectors.EVENT_READ) for key in list(self.entries.values())]
            def unregister(self, pipe):
                self.entries.pop(pipe.fileno())
            def close(self):
                self.entries.clear()
        def late_eof(_descriptor, _size):
            clock[0] = 2.0  # Shared clock passes the one-second limit during read.
            return b""
        with patch.object(q.subprocess, "Popen", return_value=Child()), \
             patch.object(q.selectors, "DefaultSelector", Selector), \
             patch.object(q.os, "set_blocking"), patch.object(q.os, "read", side_effect=late_eof), \
             patch.object(q.time, "monotonic", side_effect=lambda: clock[0]), \
             self.assertRaises(q.Refused):
            q.command(["synthetic-never-executed"], self.root, "deadline", {}, seconds=1)
        self.assertEqual(waits, [])

    def synthetic_payloads(self):
        runtime = self.root / "runtime"
        runtime.mkdir()
        files = {"migration-stream-local": b"#!/bin/sh\n# synthetic, never executed\n",
                 **{name: elf(62) for name in p.ELF_PATHS}, "bundle-manifest.json": b"{}\n"}
        for name, data in files.items():
            path = runtime / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(0o444 if name.endswith(".json") else 0o555)
        for path in reversed([runtime, *[item for item in runtime.rglob("*") if item.is_dir()]]):
            path.chmod(0o555)
        version = q.tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
        request = p.Request(runtime, self.root / "packages", "x64", "1" * 40, "2" * 64,
                            "3" * 64, "Synthetic Maintainer <fixture@example.invalid>", 1, p.POLICY)
        def verify(root):
            rows = p._snapshot(root)
            report = {"bundleRoot": str(root), "manifestSha256": "3" * 64, "productCommit": "1" * 40,
                      "productVersion": version, "sourceState": "CLEAN", "sourceSnapshotSha256": "2" * 64,
                      "target": {"os": "linux", "architecture": "x64", "targetTriple": q.LANES["x64"][1]},
                      "memberCount": 4, "totalMemberBytes": sum(len(value) for name, value in files.items() if name != "bundle-manifest.json")}
            for key, name in (("launcherSha256", "migration-stream-local"), ("runtimeControllerSha256", p.ELF_PATHS[0]),
                              ("integrityCliSha256", p.ELF_PATHS[1]), ("workbenchSha256", p.ELF_PATHS[2])):
                report[key] = rows[name][3]
            return {"schemaVersion": p.CLI_SCHEMA, "ok": True, "result": report}
        result = p._package(request, verify=verify, dependencies=lambda *_: "libc6",
                            native=lambda _: "24.04", publish=lambda stage, target: stage.rename(target))
        tar_path = next(request.destination / name for name in result["artifacts"] if name.endswith(".tar.gz"))
        deb_path = next(request.destination / name for name in result["artifacts"] if name.endswith(".deb"))
        data, offset, members = deb_path.read_bytes(), 8, {}
        while offset < len(data):
            header = data[offset:offset + 60]
            name = header[:16].decode("ascii").strip().removesuffix("/")
            size = int(header[48:58])
            offset += 60
            members[name] = data[offset:offset + size]
            offset += size + size % 2
        payload = self.root / "data.tar"
        payload.write_bytes(gzip.decompress(members["data.tar.gz"]))
        portable, installed = self.root / "portable", self.root / "installed"
        q.extract_ustar(payload, installed)
        q.extract_ustar(tar_path, portable, compressed=True)
        return runtime, portable, installed, version

    def test_actual_synthetic_package_graph_matches_both_inspection_routes(self):
        runtime, portable, installed, version = self.synthetic_payloads()
        count = q.inspect_payload(p, p._snapshot(runtime), portable, installed, adapter,
                                  "1" * 40, version, "x64")
        self.assertGreater(count, 9)

    def test_payload_inspection_refuses_runtime_mutation_and_extra_install_file(self):
        runtime, portable, installed, version = self.synthetic_payloads()
        extra = installed / "usr/share/applications/foreign.desktop"
        extra.write_bytes(b"synthetic foreign file")
        with self.assertRaises(q.Refused):
            q.inspect_payload(p, p._snapshot(runtime), portable, installed, adapter,
                              "1" * 40, version, "x64")
        extra.unlink()
        target = installed / "usr/lib/migration-stream/Runtime" / p.ELF_PATHS[0]
        target.chmod(0o644)
        target.write_bytes(b"changed synthetic member")
        with self.assertRaises(q.Refused):
            q.inspect_payload(p, p._snapshot(runtime), portable, installed, adapter,
                              "1" * 40, version, "x64")

    def test_workflow_is_manual_native_bounded_and_has_no_upload_or_install(self):
        text = (ROOT / ".github/workflows/linux-desktop-packaging-qualification.yml").read_text()
        self.assertIn("workflow_dispatch:", text)
        self.assertIn("github.run_attempt == 1", text)
        self.assertIn("github.sha == inputs.expected_source_commit", text)
        self.assertIn("timeout-minutes: 45", text)
        self.assertIn("ubuntu-24.04-arm", text)
        self.assertIn("1.99.1-aarch64-unknown-linux-gnu", text)
        self.assertIn("python3 -I -B scripts/qualify-linux-desktop-packaging.py", text)
        self.assertEqual(text.count("uses:"), 2)
        for forbidden in ("upload-artifact", "download-artifact", "workflow_run:", "schedule:", "sudo ", "apt-get", "dpkg -i"):
            self.assertNotIn(forbidden, text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
