#!/usr/bin/env python3
"""Synthetic Linux archive, tuple, delivery and workflow checks. No install or product process."""
import datetime as dt
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from contextlib import redirect_stdout
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("linux_candidate_tested", Path(__file__).with_name("linux-engineering-candidate.py"))
m = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(m)
q = m.q


class CandidateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="linux-candidate-synthetic-")
        self.root = Path(self.temp.name).resolve()
        self.version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
        self.commit = "a" * 40
        self.tree = "b" * 40
        self.run = "1234"
        self.deb_bytes = b"!<arch>\nSYNTHETIC never install\n"
        self.tar_bytes = gzip.compress(b"SYNTHETIC portable archive, never extract here")
        self.guard = mock.patch.object(subprocess, "Popen", side_effect=AssertionError("no product or git process"))
        self.guard.start()
        self.net = mock.patch("urllib.request.OpenerDirector.open", side_effect=AssertionError("no network"))
        self.net.start()

    def tearDown(self):
        self.net.stop()
        self.guard.stop()
        self.temp.cleanup()

    def lane(self, architecture):
        triple = q.LANES[architecture][1]
        deb_arch = q.LANES[architecture][2]
        debian = self.version + "+git" + self.commit[:12] + "-1"
        deb_name = "migration-stream_" + debian + "_" + deb_arch + ".deb"
        tar_name = "migration-stream-" + debian + "-linux-" + architecture + "-gnu.tar.gz"
        work = self.root / ("migration-stream-linux-qualification-" + architecture + "synth1")
        packages = work / "packages"
        packages.mkdir(parents=True)
        (packages / deb_name).write_bytes(self.deb_bytes)
        (packages / tar_name).write_bytes(self.tar_bytes)
        artifacts = {name: {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
                     for name, data in ((deb_name, self.deb_bytes), (tar_name, self.tar_bytes))}
        evidence = {"schemaVersion": "migration-stream/linux-package-qualification/v1",
                    "status": "QUALIFIED_OFFLINE_PACKAGE_PAYLOAD", "sourceCommit": self.commit,
                    "sourceTree": self.tree, "architecture": architecture, "target": triple,
                    "runId": self.run, "runAttempt": 1, "ubuntuSeries": "24.04",
                    "osInstallation": False, "appBrowserRegistration": False, "binaryUpload": False,
                    "productMetadataVerifyCalls": 6, "artifacts": artifacts,
                    "sourceSnapshotSha256": "c" * 64, "runtimeManifestSha256": "d" * 64}
        receipt = {"schemaVersion": "migration-stream/linux-desktop-package/v1", "engineeringPolicy": m.p.POLICY,
                   "package": "migration-stream", "version": debian, "architecture": deb_arch,
                   "nativeUbuntuSeries": "24.04", "productionVerifyCalls": 2, "supportedPlatformClaim": False,
                   "artifacts": artifacts, "runtimeFileGraph": {"synthetic": ["file", 0o444, 1, "e" * 64]},
                   "runtime": {"productCommit": self.commit, "productVersion": self.version, "sourceState": "CLEAN",
                               "sourceSnapshotSha256": "c" * 64, "manifestSha256": "d" * 64,
                               "target": {"os": "linux", "architecture": architecture, "targetTriple": triple}}}
        (packages / "package-receipt.json").write_bytes(json.dumps(receipt, sort_keys=True, separators=(",", ":")).encode() + b"\n")
        parent = self.root / ("migration-stream-linux-candidate-" + architecture + "-" + self.run)
        parent.mkdir()
        ctx = {"runId": self.run, "architecture": architecture, "target": triple, "sourceCommit": self.commit,
               "sourceTree": self.tree, "parent": parent, "temporary": self.root}
        return work, evidence, ctx, deb_name, tar_name

    def environment(self, architecture):
        return {"PUBLISH_LINUX_ENGINEERING_CANDIDATE": "true", "GITHUB_REF": "refs/heads/main",
                "GITHUB_REPOSITORY": m.REPO, "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_RUN_ATTEMPT": "1",
                "GITHUB_SHA": self.commit, "EXPECTED_SOURCE_COMMIT": self.commit, "GITHUB_RUN_ID": self.run,
                "QUALIFICATION_ARCHITECTURE": architecture, "RUNNER_TEMP": str(self.root),
                "RUNNER_OS": "Linux", "RUNNER_ARCH": "ARM64" if architecture == "arm64" else "X64",
                "RUSTUP_TOOLCHAIN": "1.99.1-" + q.LANES[architecture][1]}

    def handoff(self, architecture):
        work, evidence, ctx, _deb, _tar = self.lane(architecture)
        output = self.root / (architecture + "-outputs")
        output.write_text("")
        with mock.patch.dict(os.environ, {**self.environment(architecture), "GITHUB_OUTPUT": str(output)}, clear=True):
            q.publication_handoff(work, evidence)
        text = output.read_text()
        sha = text.split("qualification_sha=", 1)[1].strip()
        self.assertIn("qualified_work=" + str(work), text)
        self.assertEqual((work / "qualification.json").read_bytes(),
                         json.dumps(evidence, sort_keys=True, separators=(",", ":")).encode() + b"\n")
        self.assertFalse(json.loads((work / "qualification.json").read_text())["binaryUpload"])
        deb_name, tar_name, _version = q.package_names(evidence)
        return work, evidence, ctx, deb_name, tar_name

    def inventory(self, architecture, identifier="4567"):
        return {"total_count": 1, "artifacts": [{"id": int(identifier),
            "name": "migration-stream-linux-engineering-candidate-" + architecture + "-" + self.run,
            "digest": "sha256:" + "e" * 64, "expired": False, "size_in_bytes": len(self.deb_bytes) + len(self.tar_bytes),
            "created_at": "2026-11-01T12:00:00Z", "expires_at": "2026-11-08T12:00:00Z",
            "workflow_run": {"id": int(self.run), "head_sha": self.commit, "head_branch": "main",
                             "repository_id": m.REPO_ID, "head_repository_id": m.REPO_ID}}]}

    def test_both_tuples_stage_and_replay_exact_archive_bytes(self):
        now = dt.datetime(2026, 11, 1, 13, tzinfo=dt.timezone.utc)
        for architecture in ("x64", "arm64"):
            with self.subTest(architecture=architecture):
                work, _evidence, ctx, deb_name, tar_name = self.handoff(architecture)
                authority = m.stage(work, ctx, (work / "qualification.json").read_bytes() and
                                    hashlib.sha256((work / "qualification.json").read_bytes()).hexdigest(),
                                    publisher=os.rename)
                download = ctx["parent"] / "download"
                shutil.copytree(ctx["parent"] / "candidate", download)
                admitted = m.artifact_admission(self.inventory(architecture), ctx, "4567", "e" * 64, now)
                result = m.replay(download, authority, admitted, ctx)
                self.assertEqual(result["status"], "LINUX_CANDIDATE_PUBLISHED_PASSIVE_REPLAY")
                self.assertIs(result["uploaded"], True)
                self.assertNotIn("binaryUpload", result)
                self.assertEqual(result["target"], q.LANES[architecture][1])
                self.assertFalse(result["packageInstalled"])
                self.assertFalse(result["packageExecuted"])
                self.assertEqual((download / deb_name).read_bytes(), self.deb_bytes)
                self.assertEqual((download / tar_name).read_bytes(), self.tar_bytes)
                self.assertTrue((download / tar_name).read_bytes().startswith(b"\x1f\x8b"))
                self.assertFalse(json.loads((download / "qualification.json").read_text())["binaryUpload"])

    def test_sibling_architecture_artifact_is_admitted_and_a_third_is_not(self):
        ctx = self.lane("x64")[2]
        now = dt.datetime(2026, 11, 1, 13, tzinfo=dt.timezone.utc)
        inventory = self.inventory("x64")
        inventory["total_count"] = 2
        inventory["artifacts"].append({"id": 4568, "name": "migration-stream-linux-engineering-candidate-arm64-" + self.run,
                                       "digest": "sha256:" + "f" * 64, "expired": False, "size_in_bytes": 10,
                                       "created_at": "2026-11-01T12:00:00Z", "expires_at": "2026-11-08T12:00:00Z",
                                       "workflow_run": inventory["artifacts"][0]["workflow_run"]})
        admitted = m.artifact_admission(inventory, ctx, "4567", "e" * 64, now)
        self.assertEqual(admitted["id"], "4567")
        with self.assertRaises(ValueError):
            m.artifact_admission(inventory, ctx, "4568", "f" * 64, now)
        inventory["total_count"] = 3
        inventory["artifacts"].append(dict(inventory["artifacts"][1], id=4569, name="foreign"))
        with self.assertRaises(Exception):
            m.artifact_admission(inventory, ctx, "4567", "e" * 64, now)

    def test_wrong_tuple_digest_expiry_and_owner_refuse(self):
        _work, evidence, ctx, deb_name, _tar = self.lane("x64")
        evidence = dict(evidence, target=q.LANES["arm64"][1], architecture="arm64")
        with self.assertRaises(Exception):
            q.package_names(evidence)
        evidence = json.loads((self.root / "migration-stream-linux-qualification-x64synth1" / "packages" / "package-receipt.json").read_text())
        self.assertEqual(evidence["architecture"], "amd64")
        swapped = self.lane("arm64")[1]
        swapped["artifacts"] = {deb_name: swapped["artifacts"].popitem()[1]}
        with self.assertRaises(Exception):
            q.package_names(swapped)
        now = dt.datetime(2026, 11, 1, 13, tzinfo=dt.timezone.utc)
        for key, value in (("digest", "sha256:" + "1" * 64), ("expired", True), ("id", 99),
                           ("name", "migration-stream-linux-engineering-candidate-arm64-" + self.run),
                           ("expires_at", "2026-11-02T12:00:00Z")):
            item = self.inventory("x64")
            item["artifacts"][0][key] = value
            with self.subTest(key=key), self.assertRaises(Exception):
                m.artifact_admission(item, ctx, "4567", "e" * 64, now)
        self.assertIsNotNone(deb_name)

    def test_changed_download_extra_file_and_link_refuse(self):
        work, _evidence, ctx, deb_name, _tar = self.handoff("x64")
        sha = hashlib.sha256((work / "qualification.json").read_bytes()).hexdigest()
        authority = m.stage(work, ctx, sha, publisher=os.rename)
        download = ctx["parent"] / "download"
        shutil.copytree(ctx["parent"] / "candidate", download)
        (download / deb_name).write_bytes(b"!<arch>\nchanged synthetic\n")
        with self.assertRaises(Exception):
            m.replay(download, authority, {}, ctx)
        shutil.copyfile(work / "packages" / deb_name, download / deb_name)
        (download / "unexpected").write_bytes(b"extra")
        with self.assertRaises(Exception):
            m.replay(download, authority, {}, ctx)
        linked = ctx["parent"] / "linked"
        linked.symlink_to(work / "packages" / deb_name)
        with self.assertRaises(Exception):
            m.record(linked, 100)
        linked.unlink()
        os.link(work / "packages" / deb_name, linked)
        with self.assertRaises(Exception):
            m.record(linked, q.MAX_ARCHIVE)

    def test_handoff_requires_explicit_main_option_and_does_not_build(self):
        output = self.root / "outputs"
        work, evidence, _ctx, _deb, _tar = self.lane("x64")
        with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(output), "PUBLISH_LINUX_ENGINEERING_CANDIDATE": "false"}, clear=True):
            with self.assertRaises(Exception):
                q.publication_handoff(work, evidence)
        self.assertFalse(output.exists())
        self.assertFalse((work / "qualification.json").exists())
        captured = io.StringIO()
        with mock.patch.object(q, "qualify", side_effect=AssertionError("build")), redirect_stdout(captured):
            self.assertEqual(q.main(["--publication-handoff"]), 1)
        self.assertNotIn("QUALIFIED", captured.getvalue())

    def test_ordinary_success_prints_evidence_without_a_path_or_upload(self):
        work = self.root / "migration-stream-linux-qualification-ordinary1"
        work.mkdir()
        evidence = {"schemaVersion": "migration-stream/linux-package-qualification/v1", "binaryUpload": False, "status": "QUALIFIED_OFFLINE_PACKAGE_PAYLOAD"}
        output = self.root / "outputs"
        captured = io.StringIO()
        with mock.patch.object(q, "qualify", return_value=(evidence, work)), \
             mock.patch.object(q.signal, "signal"), mock.patch.object(q.signal, "setitimer"), \
             mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(output)}, clear=True), \
             redirect_stdout(captured):
            self.assertEqual(q.main(), 0)
        self.assertEqual(captured.getvalue(), json.dumps(evidence, sort_keys=True, separators=(",", ":")) + "\n")
        self.assertNotIn(str(work), captured.getvalue())
        self.assertFalse((work / "qualification.json").exists())
        self.assertFalse(output.exists())

    def test_context_refuses_before_git_on_this_host(self):
        with self.assertRaises(ValueError):
            m.context({"PUBLISH_LINUX_ENGINEERING_CANDIDATE": "false"}, "--stage")
        env = self.environment("arm64")
        env["GITHUB_REF"] = "refs/heads/feature"
        with self.assertRaises(ValueError):
            m.context(env, "--stage")
        self.assertFalse((self.root / ("migration-stream-linux-candidate-arm64-" + self.run)).exists())

    def test_metadata_client_is_bounded_and_has_no_redirect(self):
        self.net.stop()
        payload = json.dumps(self.inventory("x64")).encode()
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.status = 200
        response.read.return_value = payload
        opener = mock.Mock()
        opener.open.return_value = response
        self.assertEqual(m.metadata_get(opener, "SYNTHETIC_TOKEN", self.run)["total_count"], 1)
        request = opener.open.call_args.args[0]
        self.assertEqual(request.full_url, "https://api.github.com/repos/" + m.REPO + "/actions/runs/" + self.run + "/artifacts?per_page=2")
        self.assertEqual(request.get_header("User-agent"), "migration-stream-linux-candidate")
        self.assertNotIn("SYNTHETIC_TOKEN", json.dumps(m.metadata_get(opener, "SYNTHETIC_TOKEN", self.run)))
        response.read.return_value = b"x" * (m.MAX_JSON + 1)
        with self.assertRaises(Exception):
            m.metadata_get(opener, "SYNTHETIC_TOKEN", self.run)
        with self.assertRaises(Exception):
            m.NoRedirect().redirect_request(None, None, None, None, None, None)
        self.net.start()

    def test_driver_and_workflow_keep_one_build_and_pinned_delivery(self):
        workflow = (ROOT / ".github/workflows/linux-desktop-packaging-qualification.yml").read_text()
        driver = (ROOT / "scripts/qualify-linux-desktop-packaging.py").read_text()
        helper = Path(m.__file__).read_text()
        self.assertEqual(workflow.count("python3 -I -B scripts/qualify-linux-desktop-packaging.py --publication-handoff"), 1)
        self.assertEqual(workflow.count("python3 -I -B scripts/qualify-linux-desktop-packaging.py\n"), 1)
        self.assertLess(workflow.index("= true ]; then"), workflow.index("--publication-handoff"))
        self.assertLess(workflow.index("--publication-handoff"), workflow.index("\n          else\n"))
        for required in ("default: false", "retention-days: 7", "overwrite: false", "include-hidden-files: false",
                         "archive: true", "digest-mismatch: error", "if-no-files-found: error",
                         "artifact-ids: ${{ steps.upload_candidate.outputs.artifact-id }}",
                         "migration-stream-linux-engineering-candidate-${{ matrix.architecture }}-${{ github.run_id }}",
                         "fail-fast: false", "cancel-in-progress: false", "contents: read", "actions: read"):
            self.assertIn(required, workflow)
        self.assertNotIn("actions: write", workflow)
        self.assertNotIn("path: ${{ runner.temp }}\n", workflow)
        self.assertNotIn("secrets.", workflow)
        self.assertEqual(workflow.count("uses: actions/upload-artifact@"), 1)
        self.assertEqual(workflow.count("uses: actions/download-artifact@"), 1)
        self.assertIn('CARGO_NET_RETRY": "0"', driver)
        self.assertIn('npm_config_fetch_retries": "0"', driver)
        self.assertIn('if label == "rust-preparation":', driver)
        self.assertEqual(helper.count("subprocess.run"), 1)
        for forbidden in ("dpkg", "cargo", "npm", "rustup", "apt-get"):
            self.assertNotIn(forbidden, helper)
        ci = (ROOT / ".github/workflows/public-source-ci.yml").read_text()
        self.assertEqual(ci.count("scripts/linux-engineering-candidate-self-test.py"), 1)
        self.assertEqual(ci.count("scripts/windows-engineering-candidate-self-test.py"), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
