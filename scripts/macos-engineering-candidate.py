#!/usr/bin/env python3
"""Stage/replay a qualified Mac DMG and two receipts; never attach or execute it."""
from __future__ import annotations

import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import stat
import sys
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("mac_candidate_qualification", ROOT / "scripts/macos-engineering-qualification.py")
q = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(q)
p, a = q.p, q.a
FILES = (p.IMAGE_NAME, p.RECEIPT_NAME, "qualification.json")
REPO = "tzimiswcc/wcc-migration-stream"
REPO_ID = 1329975731
SCHEMA = "migration-stream/macos-engineering-candidate/v1"


def require(value):
    if not value:
        raise ValueError("Mac candidate refused")


def digest(value):
    return type(value) is str and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def record(path, maximum, *, copy_to=None):
    a._real_path(path)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    output = None
    try:
        before = os.fstat(descriptor)
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and 0 < before.st_size <= maximum)
        if copy_to is not None:
            output = copy_to.open("xb")
        count, sha = 0, hashlib.sha256()
        while chunk := os.read(descriptor, 65536):
            count += len(chunk)
            require(count <= before.st_size and count <= maximum)
            sha.update(chunk)
            if output is not None:
                output.write(chunk)
        require(count == before.st_size and a._signature(before) == a._signature(os.fstat(descriptor))
                and a._signature(before) == a._signature(path.lstat()))
        if output is not None:
            output.flush()
            os.fsync(output.fileno())
        return {"bytes": count, "sha256": sha.hexdigest()}
    finally:
        os.close(descriptor)
        if output is not None:
            output.close()


def limit(name):
    return p.MAX_IMAGE if name == p.IMAGE_NAME else q.MAX_PLIST


def file_set(directory):
    a._real_path(directory)
    require(directory.is_dir())
    names = set()
    with os.scandir(directory) as entries:
        for entry in entries:
            require(len(names) < len(FILES))
            names.add(entry.name)
    require(names == set(FILES))


def context(environment, phase):
    require(sys.platform == "darwin" and platform.machine() == "arm64")
    require(environment.get("PUBLISH_MACOS_ENGINEERING_CANDIDATE") == "true")
    require(environment.get("GITHUB_EVENT_NAME") == "workflow_dispatch"
            and environment.get("GITHUB_RUN_ATTEMPT") == "1"
            and environment.get("GITHUB_REF") == "refs/heads/main"
            and environment.get("GITHUB_REPOSITORY") == REPO)
    expected, run = environment.get("EXPECTED_SOURCE_COMMIT", ""), environment.get("GITHUB_RUN_ID", "")
    require(re.fullmatch(r"[0-9a-f]{40}", expected) is not None
            and environment.get("GITHUB_SHA") == expected and re.fullmatch(r"[0-9]{1,20}", run) is not None)
    temporary = Path(environment["RUNNER_TEMP"])
    q.directory_id(temporary)
    parent = temporary / ("migration-stream-macos-candidate-" + run)
    if phase == "--stage":
        a._real_path(parent, absent=True)
        parent.mkdir(mode=0o700)
    q.directory_id(parent)
    logs = Path(tempfile.mkdtemp(prefix="context-", dir=parent))
    runner = q.Runner(logs)
    q.source_identity(ROOT, expected, runner)
    require(runner(["/usr/bin/git", "rev-parse", "refs/remotes/origin/main"], cwd=ROOT).decode().strip() == expected)
    tree = runner(["/usr/bin/git", "rev-parse", "HEAD^{tree}"], cwd=ROOT).decode().strip()
    require(re.fullmatch(r"[0-9a-f]{40}", tree) is not None)
    return {"runId": run, "sourceCommit": expected, "sourceTree": tree, "parent": parent, "temporary": temporary}


def sources(work):
    return {name: work / (name if name == "qualification.json" else "package/" + name) for name in FILES}


def validate(qualification, package, ctx, records):
    require(type(qualification) is dict and type(package) is dict)
    require(qualification.get("schemaVersion") == q.SCHEMA
            and qualification.get("status") == "ENGINEERING_IMAGE_QUALIFIED"
            and qualification.get("sourceCommit") == ctx["sourceCommit"]
            and qualification.get("target") == "aarch64-apple-darwin"
            and qualification.get("qualificationHost") == "macos-26-arm64")
    for key in ("mountedRuntimeVerified", "privateCopyRuntimeVerified", "mountedGraphAndModesEqual", "privateCopyGraphAndModesEqual"):
        require(qualification.get(key) is True)
    for key in ("appLaunched", "installed", "signed", "notarized", "uploaded"):
        require(qualification.get(key) is False)
    for key, value in (("packagingRuntimeVerifyCalls", 4), ("mountedRuntimeVerifyCalls", 1), ("privateCopyRuntimeVerifyCalls", 1)):
        require(type(qualification.get(key)) is int and qualification[key] == value)
    require(qualification.get("packageReceiptSha256") == records[p.RECEIPT_NAME]["sha256"]
            and qualification.get("image") == records[p.IMAGE_NAME])
    require(package.get("schemaVersion") == p.SCHEMA and package.get("policy") == a.POLICY
            and package.get("architecture") == "arm64")
    for key in ("signingPerformed", "notarizationPerformed", "installationPerformed", "explicitImageAttachment", "mountedContentsQualified"):
        require(package.get(key) is False)
    require(package.get("image") == {"name": p.IMAGE_NAME, **records[p.IMAGE_NAME], **p.IMAGE_INFO})
    runtime, app = package.get("runtime", {}), package.get("app", {})
    require(runtime.get("productCommit") == ctx["sourceCommit"] and runtime.get("sourceState") == "CLEAN"
            and app.get("name") == p.APP_NAME and digest(app.get("wrapperSha256"))
            and qualification.get("wrapperSha256") == app["wrapperSha256"])
    require(type(app.get("entries")) is dict and 0 < len(app["entries"]) <= 4096
            and type(qualification.get("appEntries")) is int and qualification["appEntries"] == len(app["entries"]))


def current_sources(work, ctx):
    a._real_path(work)
    require(work.parent == ctx["temporary"]
            and re.fullmatch(r"migration-stream-macos-qualification-[A-Za-z0-9_-]{8,32}", work.name) is not None)
    paths = sources(work)
    records = {name: record(path, limit(name)) for name, path in paths.items()}
    qualification, package = q.read_json(paths["qualification.json"]), q.read_json(paths[p.RECEIPT_NAME])
    validate(qualification, package, ctx, records)
    p._package_names(work / "package")
    require(a._contents(a._snapshot(work / "package" / p.APP_NAME))
            == {name: tuple(row) for name, row in package["app"]["entries"].items()})
    require({name: record(path, limit(name)) for name, path in paths.items()} == records)
    return records


def stage(work, ctx, qualification_sha, *, publisher=a._publish_noreplace):
    require(digest(qualification_sha))
    records = current_sources(work, ctx)
    require(records["qualification.json"]["sha256"] == qualification_sha)
    destination = ctx["parent"] / "candidate"
    a._real_path(destination, absent=True)
    staging = Path(tempfile.mkdtemp(prefix=".candidate-", dir=destination.parent))
    for name, path in sources(work).items():
        require(record(path, limit(name), copy_to=staging / name) == records[name])
    require(current_sources(work, ctx) == records)
    file_set(staging)
    publisher(staging, destination)
    file_set(destination)
    require({name: record(destination / name, limit(name)) for name in FILES} == records)
    return {"schemaVersion": SCHEMA, "runId": ctx["runId"], "sourceCommit": ctx["sourceCommit"],
            "sourceTree": ctx["sourceTree"], "qualifiedWork": str(work), "files": records}


def artifact_admission(inventory, ctx, identifier, sha, now):
    require(re.fullmatch(r"[0-9]{1,20}", identifier) is not None and digest(sha))
    require(type(inventory) is dict and type(inventory.get("total_count")) is int and inventory["total_count"] == 1
            and type(inventory.get("artifacts")) is list and len(inventory["artifacts"]) == 1)
    item = inventory["artifacts"][0]
    require(type(item) is dict and type(item.get("id")) is int and item["id"] == int(identifier)
            and item.get("name") == "migration-stream-macos-engineering-candidate-" + ctx["runId"]
            and item.get("digest") == "sha256:" + sha and item.get("expired") is False
            and type(item.get("size_in_bytes")) is int and 0 < item["size_in_bytes"] <= p.MAX_IMAGE + 4 * q.MAX_PLIST)
    owner = item.get("workflow_run", {})
    require(type(owner.get("id")) is int and owner["id"] == int(ctx["runId"])
            and owner.get("head_sha") == ctx["sourceCommit"] and owner.get("head_branch") == "main"
            and owner.get("repository_id") == REPO_ID and owner.get("head_repository_id") == REPO_ID)
    created, expires = (dt.datetime.fromisoformat(item[key].replace("Z", "+00:00")) for key in ("created_at", "expires_at"))
    require(now.tzinfo is not None and created.tzinfo is not None and expires.tzinfo is not None
            and created <= now < expires and abs((expires - created).total_seconds() - 7 * 86400) <= 300)
    return {"id": identifier, "sha256": sha, "bytes": item["size_in_bytes"], "expiresAt": item["expires_at"]}


def replay(download, authority, artifact, ctx):
    require(authority.get("schemaVersion") == SCHEMA and authority.get("runId") == ctx["runId"]
            and authority.get("sourceCommit") == ctx["sourceCommit"] and authority.get("sourceTree") == ctx["sourceTree"]
            and set(authority.get("files", {})) == set(FILES))
    records = current_sources(Path(authority["qualifiedWork"]), ctx)
    require(records == authority["files"])
    for directory in (ctx["parent"] / "candidate", download):
        file_set(directory)
        require({name: record(directory / name, limit(name)) for name in FILES} == records)
        validate(q.read_json(directory / "qualification.json"), q.read_json(directory / p.RECEIPT_NAME), ctx, records)
    require(current_sources(Path(authority["qualifiedWork"]), ctx) == records)
    return {"schemaVersion": SCHEMA, "status": "MACOS_CANDIDATE_PUBLISHED_PASSIVE_REPLAY", "runId": ctx["runId"],
            "sourceCommit": ctx["sourceCommit"], "sourceTree": ctx["sourceTree"], "files": records,
            "artifact": artifact, "retentionDays": 7, "uploaded": True, "appLaunched": False,
            "installed": False, "signed": False, "notarized": False, "postDownloadImageAttached": False}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        raise ValueError("Mac candidate metadata redirect refused")


def metadata_get(opener, token, run, clock=time.monotonic):
    require(type(token) is str and 0 < len(token) <= 4096 and re.fullmatch(r"[0-9]{1,20}", run) is not None)
    request = urllib.request.Request("https://api.github.com/repos/" + REPO + "/actions/runs/" + run + "/artifacts?per_page=2",
                                    headers={"Authorization": "Bearer " + token, "Accept": "application/vnd.github+json",
                                             "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "migration-stream-macos-candidate"})
    deadline = clock() + 15
    with opener.open(request, timeout=10) as response:
        require(type(response.status) is int and response.status == 200)
        raw = response.read(q.MAX_PLIST + 1)
        require(len(raw) <= q.MAX_PLIST and clock() < deadline)
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result)
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=pairs)


def outputs(values):
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
        for key, value in values.items():
            require(re.fullmatch(r"[a-z_]+", key) is not None and re.fullmatch(r"[A-Za-z0-9._-]{1,128}", str(value)) is not None)
            stream.write(key + "=" + str(value) + "\n")


def trusted_json(path, sha):
    require(digest(sha) and record(path, q.MAX_PLIST)["sha256"] == sha)
    return q.read_json(path)


def main():
    try:
        require(sys.argv[1:] in (["--stage"], ["--admit-artifact"], ["--replay"]))
        phase = sys.argv[1]
        ctx = context(dict(os.environ), phase)
        parent = ctx["parent"]
        if phase == "--stage":
            authority = stage(Path(os.environ["QUALIFIED_WORKSPACE"]), ctx, os.environ.get("QUALIFICATION_SHA"))
            path = parent / "candidate-authority.json"
            q.write_json(path, authority)
            outputs({"candidate_ready": "true", "authority_sha": record(path, q.MAX_PLIST)["sha256"]})
            print('{"status":"MACOS_CANDIDATE_STAGED","uploaded":false}')
        else:
            identifier, sha = os.environ["CANDIDATE_ARTIFACT_ID"], os.environ["CANDIDATE_ARTIFACT_DIGEST"]
            if phase == "--admit-artifact":
                inventory = metadata_get(urllib.request.build_opener(NoRedirect), os.environ["GH_TOKEN"], ctx["runId"])
                admitted = artifact_admission(inventory, ctx, identifier, sha, dt.datetime.now(dt.timezone.utc))
                path = parent / "candidate-artifact.json"
                q.write_json(path, {"inventory": inventory, "admitted": admitted})
                outputs({"artifact_metadata_sha": record(path, q.MAX_PLIST)["sha256"]})
            else:
                metadata = trusted_json(parent / "candidate-artifact.json", os.environ.get("CANDIDATE_METADATA_SHA"))
                admitted = artifact_admission(metadata["inventory"], ctx, identifier, sha, dt.datetime.now(dt.timezone.utc))
                require(admitted == metadata["admitted"])
                authority = trusted_json(parent / "candidate-authority.json", os.environ.get("CANDIDATE_AUTHORITY_SHA"))
                result = replay(parent / "download", authority, admitted, ctx)
                q.write_json(parent / "publication.json", result)
                print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except Exception:
        print('{"status":"MACOS_CANDIDATE_REFUSED"}')
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
