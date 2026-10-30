#!/usr/bin/env python3
"""Offline publication failure cases using nonrunnable synthetic files only."""
import copy
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("mac_candidate_tested", Path(__file__).with_name("macos-engineering-candidate.py"))
m = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(m)


class CandidateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="mac-candidate-synthetic-")
        self.root = Path(self.temp.name).resolve()
        self.parent = self.root / "candidate-run"
        self.parent.mkdir()
        self.work = self.root / "migration-stream-macos-qualification-synthetic001"
        self.package = self.work / "package"
        self.package.mkdir(parents=True)
        self.app = self.package / m.p.APP_NAME
        (self.app / "Contents").mkdir(parents=True)
        (self.app / "Contents/nonrunnable").write_bytes(b"SYNTHETIC never execute")
        self.image = self.package / m.p.IMAGE_NAME
        self.image.write_bytes(b"SYNTHETIC not a disk image")
        self.ctx = {"runId": "1234", "sourceCommit": "a" * 40, "sourceTree": "b" * 40,
                    "parent": self.parent, "temporary": self.root}
        image = m.record(self.image, m.p.MAX_IMAGE)
        self.package_value = {"schemaVersion": m.p.SCHEMA, "policy": m.a.POLICY, "architecture": "arm64",
            "runtime": {"productCommit": "a" * 40, "sourceState": "CLEAN"},
            "app": {"name": m.p.APP_NAME, "entries": m.a._contents(m.a._snapshot(self.app)), "wrapperSha256": "c" * 64},
            "image": {"name": m.p.IMAGE_NAME, **image, **m.p.IMAGE_INFO},
            **{key: False for key in ("signingPerformed", "notarizationPerformed", "installationPerformed",
                                     "explicitImageAttachment", "mountedContentsQualified")}}
        m.q.write_json(self.package / m.p.RECEIPT_NAME, self.package_value)
        self.qualification = {"schemaVersion": m.q.SCHEMA, "status": "ENGINEERING_IMAGE_QUALIFIED",
            "sourceCommit": "a" * 40, "target": "aarch64-apple-darwin", "qualificationHost": "macos-26-arm64",
            **{key: True for key in ("mountedRuntimeVerified", "privateCopyRuntimeVerified", "mountedGraphAndModesEqual", "privateCopyGraphAndModesEqual")},
            **{key: False for key in ("appLaunched", "installed", "signed", "notarized", "uploaded")},
            "packagingRuntimeVerifyCalls": 4, "mountedRuntimeVerifyCalls": 1, "privateCopyRuntimeVerifyCalls": 1,
            "packageReceiptSha256": m.record(self.package / m.p.RECEIPT_NAME, m.q.MAX_PLIST)["sha256"],
            "wrapperSha256": "c" * 64, "image": image, "appEntries": len(self.package_value["app"]["entries"])}
        m.q.write_json(self.work / "qualification.json", self.qualification)
        self.qualification_sha = m.record(self.work / "qualification.json", m.q.MAX_PLIST)["sha256"]
        self.guard = mock.patch.object(m.q.subprocess, "Popen", side_effect=AssertionError("no product/process in synthetic tests"))
        self.guard.start()
        self.net_guard = mock.patch("urllib.request.OpenerDirector.open", side_effect=AssertionError("no network in synthetic tests"))
        self.net_guard.start()

    def tearDown(self):
        self.net_guard.stop()
        self.guard.stop()
        self.temp.cleanup()

    def stage(self):
        return m.stage(self.work, self.ctx, self.qualification_sha, publisher=os.rename)

    def download(self):
        target = self.parent / "download"
        shutil.copytree(self.parent / "candidate", target)
        return target

    def inventory(self):
        return {"total_count": 1, "artifacts": [{"id": 4567, "name": "migration-stream-macos-engineering-candidate-1234",
            "digest": "sha256:" + "d" * 64, "expired": False, "size_in_bytes": 1234,
            "created_at": "2026-10-30T18:00:00Z", "expires_at": "2026-11-06T18:00:00Z",
            "workflow_run": {"id": 1234, "head_sha": "a" * 40, "head_branch": "main",
                             "repository_id": m.REPO_ID, "head_repository_id": m.REPO_ID}}]}

    def test_complete_passive_staging_and_download_replay(self):
        authority = self.stage()
        inventory = self.inventory()
        artifact = m.artifact_admission(inventory, self.ctx, "4567", "d" * 64, dt.datetime(2026, 10, 30, 19, tzinfo=dt.timezone.utc))
        result = m.replay(self.download(), authority, artifact, self.ctx)
        self.assertEqual(result["status"], "MACOS_CANDIDATE_PUBLISHED_PASSIVE_REPLAY")
        self.assertFalse(result["postDownloadImageAttached"])
        self.assertEqual(set(result["files"]), set(m.FILES))
        self.assertFalse(m.q.read_json(self.work / "qualification.json")["uploaded"])

    def test_no_overwrite_or_extra_staged_file(self):
        self.stage()
        with self.assertRaises(Exception): self.stage()
        (self.parent / "candidate/private.log").write_text("NEVER UPLOAD")
        with self.assertRaises(Exception): m.file_set(self.parent / "candidate")

    def test_mismatched_handoff_never_stages(self):
        with self.assertRaises(Exception): m.stage(self.work, self.ctx, "0" * 64, publisher=os.rename)
        self.assertFalse((self.parent / "candidate").exists())

    def test_qualified_source_path_and_package_graph_are_rechecked(self):
        (self.app / "Contents/nonrunnable").write_bytes(b"changed")
        with self.assertRaises(Exception): self.stage()
        foreign = dict(self.ctx, temporary=self.parent)
        with self.assertRaises(Exception): m.current_sources(self.work, foreign)

    def test_changed_download_or_original_refuses(self):
        authority = self.stage()
        download = self.download()
        (download / m.p.IMAGE_NAME).write_bytes(b"corrupt download")
        with self.assertRaises(Exception): m.replay(download, authority, {}, self.ctx)
        shutil.copyfile(self.image, download / m.p.IMAGE_NAME)
        self.image.write_bytes(b"source changed after upload")
        with self.assertRaises(Exception): m.replay(download, authority, {}, self.ctx)

    def test_changed_local_stage_extra_download_and_wrong_context_refuse(self):
        authority = self.stage()
        download = self.download()
        (download / "unexpected").write_bytes(b"extra")
        with self.assertRaises(Exception): m.replay(download, authority, {}, self.ctx)
        (download / "unexpected").unlink()
        with self.assertRaises(Exception): m.replay(download, authority, {}, dict(self.ctx, runId="999"))
        (self.parent / "candidate" / m.p.IMAGE_NAME).write_bytes(b"changed staging")
        with self.assertRaises(Exception): m.replay(download, authority, {}, self.ctx)

    def test_qualification_claims_and_boolean_types_refuse(self):
        records = {name: m.record(path, m.limit(name)) for name, path in m.sources(self.work).items()}
        for key, value in (("uploaded", True), ("appLaunched", True), ("mountedRuntimeVerified", False),
                           ("sourceCommit", "f" * 40), ("target", "x86_64-apple-darwin"),
                           ("mountedRuntimeVerifyCalls", True), ("packageReceiptSha256", "f" * 64)):
            qualification = dict(self.qualification, **{key: value})
            with self.subTest(key=key), self.assertRaises(Exception):
                m.validate(qualification, self.package_value, self.ctx, records)

    def test_package_claims_refuse(self):
        records = {name: m.record(path, m.limit(name)) for name, path in m.sources(self.work).items()}
        for key, value in (("signingPerformed", True), ("architecture", "x64"), ("policy", "other")):
            with self.subTest(key=key), self.assertRaises(Exception):
                m.validate(self.qualification, dict(self.package_value, **{key: value}), self.ctx, records)

    def test_links_special_files_and_bounds_refuse(self):
        linked = self.parent / "linked"
        linked.symlink_to(self.image)
        with self.assertRaises(Exception): m.record(linked, m.p.MAX_IMAGE)
        linked.unlink()
        os.link(self.image, linked)
        with self.assertRaises(Exception): m.record(linked, m.p.MAX_IMAGE)
        linked.unlink()
        with self.assertRaises(Exception): m.record(self.image, 1)
        fifo = self.parent / "fifo"
        os.mkfifo(fifo)
        with self.assertRaises(Exception): m.record(fifo, m.p.MAX_IMAGE)

    def test_duplicate_receipt_keys_refuse(self):
        (self.work / "qualification.json").write_text('{"status":"x","status":"y"}')
        with self.assertRaises(Exception): self.stage()

    def test_artifact_identity_expiry_and_retention_refuse(self):
        now = dt.datetime(2026, 10, 30, 19, tzinfo=dt.timezone.utc)
        variants = []
        for key, value in (("id", True), ("expired", True), ("digest", "sha256:" + "e" * 64),
                           ("expires_at", "2026-10-31T18:00:00Z"), ("size_in_bytes", True),
                           ("name", "foreign")):
            item = self.inventory(); item["artifacts"][0][key] = value; variants.append(item)
        for key, value in (("id", 2), ("head_sha", "f" * 40), ("head_branch", "other"),
                           ("repository_id", 2), ("head_repository_id", 2)):
            item = self.inventory(); item["artifacts"][0]["workflow_run"][key] = value; variants.append(item)
        item = self.inventory(); item["total_count"] = 2; variants.append(item)
        for item in variants:
            with self.subTest(item=item), self.assertRaises(Exception):
                m.artifact_admission(item, self.ctx, "4567", "d" * 64, now)
        with self.assertRaises(Exception): m.artifact_admission(self.inventory(), self.ctx, "4567", "d" * 64, now + dt.timedelta(days=8))

    def test_metadata_client_is_fixed_bounded_and_no_redirect(self):
        payload = json.dumps(self.inventory()).encode()
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.status = 200
        response.read.return_value = payload
        opener = mock.Mock()
        opener.open.return_value = response
        value = m.metadata_get(opener, "SYNTHETIC_TOKEN", "1234")
        self.assertEqual(value, self.inventory())
        request = opener.open.call_args.args[0]
        self.assertEqual(request.full_url, "https://api.github.com/repos/" + m.REPO + "/actions/runs/1234/artifacts?per_page=2")
        self.assertEqual(opener.open.call_args.kwargs, {"timeout": 10})
        response.read.return_value = b"x" * (m.q.MAX_PLIST + 1)
        with self.assertRaises(Exception): m.metadata_get(opener, "SYNTHETIC_TOKEN", "1234")
        response.read.return_value = payload
        with self.assertRaises(Exception): m.metadata_get(opener, "SYNTHETIC_TOKEN", "1234", clock=mock.Mock(side_effect=[0, 16]))
        with self.assertRaises(Exception): m.NoRedirect().redirect_request(None, None, None, None, None, None)

    def test_publication_context_refuses_missing_option_and_non_main(self):
        env = {"PUBLISH_MACOS_ENGINEERING_CANDIDATE": "false"}
        with mock.patch.object(m.sys, "platform", "darwin"), mock.patch.object(m.platform, "machine", return_value="arm64"):
            with self.assertRaises(Exception): m.context(env, "--stage")
            env.update(PUBLISH_MACOS_ENGINEERING_CANDIDATE="true", GITHUB_REF="refs/heads/feature")
            with self.assertRaises(Exception): m.context(env, "--stage")

    def test_handoff_needs_explicit_main_option(self):
        output = self.parent / "outputs"
        environment = {"GITHUB_OUTPUT": str(output), "PUBLISH_MACOS_ENGINEERING_CANDIDATE": "false"}
        with mock.patch.dict(os.environ, environment, clear=True):
            with self.assertRaises(Exception): m.q.publication_handoff(self.work)
        self.assertFalse(output.exists())
        environment.update(PUBLISH_MACOS_ENGINEERING_CANDIDATE="true", GITHUB_REF="refs/heads/main", GITHUB_REPOSITORY=m.REPO)
        with mock.patch.dict(os.environ, environment, clear=True): m.q.publication_handoff(self.work)
        self.assertIn("qualification_sha=" + self.qualification_sha, output.read_text())
        self.assertIn("qualified_work=" + str(self.work), output.read_text())

    def test_workflow_uses_only_fixed_paths_pins_and_returned_id(self):
        text = (m.ROOT / ".github/workflows/macos-engineering-qualification.yml").read_text()
        for required in ("default: false", "retention-days: 7", "overwrite: false", "include-hidden-files: false",
                         "archive: true", "digest-mismatch: error", "artifact-ids: ${{ steps.upload_candidate.outputs.artifact-id }}",
                         "actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9",
                         "actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333"):
            self.assertIn(required, text)
        self.assertNotIn("path: ${{ runner.temp }}\n", text)
        self.assertNotIn("secrets.", text)


if __name__ == "__main__":
    unittest.main()
