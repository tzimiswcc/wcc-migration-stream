#!/usr/bin/env python3
"""Focused compiler-only loader workaround regressions; no Cargo build required."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("loader", ROOT / "rustc-macos-loader.py")
LOADER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LOADER)


class LoaderTests(unittest.TestCase):
    def test_exact_compiler_child_environment_preserves_parent_and_options(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "lib").mkdir()
            (root / "lib/libLLVM.dylib").touch()
            parent = {"DYLD_LIBRARY_PATH": "/existing", "RUSTFLAGS": "unchanged"}
            arguments = ["/compiler/rustc", "-C", "opt-level=3"]
            with patch.object(LOADER.subprocess, "check_output", side_effect=[
                "rustc 1.99.1 (official)", str(root)
            ]) as queries:
                child = LOADER.compiler_environment(arguments, parent, "darwin")
            self.assertEqual(child["DYLD_LIBRARY_PATH"], str(root / "lib") + ":/existing")
            self.assertEqual(parent["DYLD_LIBRARY_PATH"], "/existing")
            self.assertEqual(child["RUSTFLAGS"], "unchanged")
            self.assertEqual(arguments, ["/compiler/rustc", "-C", "opt-level=3"])
            self.assertEqual(queries.call_args_list[1].args[0], ["/compiler/rustc", "--print", "sysroot"])

    def test_queries_drop_jobserver_metadata_but_actual_exec_preserves_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "lib").mkdir()
            (root / "lib/libLLVM.dylib").touch()
            jobserver = {"CARGO_MAKEFLAGS": "--jobserver-auth=3,5 -j",
                         "MAKEFLAGS": "--jobserver-auth=3,5", "MFLAGS": "-j"}
            parent = {**jobserver, "RUSTFLAGS": "unchanged"}
            with patch.object(LOADER.subprocess, "check_output", side_effect=[
                "rustc 1.99.1 (official)", str(root)
            ]) as queries, patch.object(LOADER.os, "environ", parent), patch.object(
                LOADER.sys, "platform", "darwin"
            ), patch.object(LOADER.sys, "argv", ["wrapper", "/actual/rustc", "-C", "opt-level=3"]), patch.object(
                LOADER.os, "execvpe"
            ) as execute:
                LOADER.main()
            for query in queries.call_args_list:
                self.assertTrue(set(jobserver).isdisjoint(query.kwargs["env"]))
                self.assertEqual(query.kwargs["env"]["RUSTFLAGS"], "unchanged")
            self.assertEqual(execute.call_args.args[0:2], ("/actual/rustc", ["/actual/rustc", "-C", "opt-level=3"]))
            for name, value in jobserver.items():
                self.assertEqual(execute.call_args.args[2][name], value)
                self.assertEqual(parent[name], value)

    def test_other_platform_and_newer_toolchain_pass_through(self):
        parent = {"RUSTFLAGS": "unchanged"}
        with patch.object(LOADER.subprocess, "check_output") as query:
            self.assertEqual(LOADER.compiler_environment(["rustc"], parent, "linux"), parent)
            query.assert_not_called()
        with patch.object(LOADER.subprocess, "check_output", return_value="rustc 1.100.0 (next)") as query:
            self.assertEqual(LOADER.compiler_environment(["rustc"], parent, "darwin"), parent)
            self.assertEqual(query.call_count, 1)

    def test_missing_bundled_library_fails_explicitly(self):
        with patch.object(LOADER.subprocess, "check_output", side_effect=[
            "rustc 1.99.1 (official)", "/nonexistent/compiler/sysroot"
        ]):
            with self.assertRaisesRegex(RuntimeError, "lacks its bundled"):
                LOADER.compiler_environment(["rustc"], {}, "darwin")

    def test_helper_scopes_wrapper_and_rejects_existing_wrappers(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, body in {
                "uname": '#!/bin/sh\nprintf "%s\\n" "${PROBE_PLATFORM:-Darwin}"\n',
                "rustc": '#!/bin/sh\nprintf "%s\\n" "${PROBE_RUST:-rustc 1.99.1 (official)}"\n',
                "cargo": '#!' + sys.executable + '\nimport os,json,sys\nprint(json.dumps({"wrapper":os.environ.get("RUSTC_WRAPPER"),"loader":os.environ.get("DYLD_LIBRARY_PATH"),"args":sys.argv[1:]}))\n',
            }.items():
                path = root / name
                path.write_text(body)
                path.chmod(0o755)
            environment = dict(os.environ)
            for name in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "DYLD_LIBRARY_PATH"):
                environment.pop(name, None)
            environment["PATH"] = str(root) + os.pathsep + environment["PATH"]
            command = ["bash", str(ROOT / "cargo-build.sh"), "run", "--release", "--", "app-argument"]
            def execute(extra):
                return subprocess.run(command, env={**environment, **extra}, capture_output=True, text=True)
            result = execute({})
            self.assertEqual(result.returncode, 0, result.stderr)
            observed = json.loads(result.stdout)
            self.assertEqual(observed["wrapper"], str(ROOT / "rustc-macos-loader.py"))
            self.assertIsNone(observed["loader"], "Cargo/application parent must receive no new loader path")
            self.assertEqual(observed["args"], ["run", "--release", "--", "app-argument"])
            for name in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
                result = execute({name: "/user/wrapper"})
                self.assertEqual(result.returncode, 2)
                self.assertIn("cannot safely compose", result.stderr)
                self.assertFalse(result.stdout)
            for extra in ({"PROBE_PLATFORM": "Linux"}, {"PROBE_RUST": "rustc 1.100.0 (next)"}):
                result = execute(extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIsNone(json.loads(result.stdout)["wrapper"])


if __name__ == "__main__":
    unittest.main()
