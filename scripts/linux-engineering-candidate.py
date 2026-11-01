#!/usr/bin/env python3
"""Stage and replay one qualified Linux deb and portable archive. Never install or execute them."""
from __future__ import annotations

import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
REPO = "tzimiswcc/wcc-migration-stream"
REPO_ID = 1329975731
SCHEMA = "migration-stream/linux-engineering-candidate/v1"
MAX_JSON = 1024 * 1024


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


q = load("linux_candidate_qualification", "scripts/qualify-linux-desktop-packaging.py")
p = load("linux_candidate_packager", "scripts/package-linux-desktop.py")


def require(value):
    if not value:
        raise ValueError("Linux candidate refused")


def digest(value):
    return type(value) is str and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def record(path, maximum):
    p._real_path(path)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and 0 < info.st_size <= maximum)
        count, sha = 0, hashlib.sha256()
        while chunk := os.read(descriptor, 65536):
            count += len(chunk)
            require(count <= info.st_size and count <= maximum)
            sha.update(chunk)
        require(count == info.st_size and p._signature(info) == p._signature(os.fstat(descriptor))
                and p._signature(info) == p._signature(path.lstat()))
        return {"bytes": count, "sha256": sha.hexdigest()}
    finally:
        os.close(descriptor)


def copy_record(source, destination, expected, maximum):
    require(record(source, maximum) == expected)
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    output = destination.open("xb")
    try:
        count, sha = 0, hashlib.sha256()
        while chunk := os.read(descriptor, 65536):
            count += len(chunk)
            require(count <= expected["bytes"])
            sha.update(chunk)
            output.write(chunk)
        output.flush()
        os.fsync(output.fileno())
        require(count == expected["bytes"] and sha.hexdigest() == expected["sha256"])
    finally:
        output.close()
        os.close(descriptor)
    require(record(destination, maximum) == expected)


def read_json(path, maximum):
    row = record(path, maximum)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        body = os.read(descriptor, maximum + 1)
        require(len(body) == row["bytes"] and hashlib.sha256(body).hexdigest() == row["sha256"])
    finally:
        os.close(descriptor)
    return p._json(body), row


def names_from(qualification):
    deb_name, tar_name, version = q.package_names(qualification)
    return (deb_name, tar_name, "package-receipt.json", "qualification.json"), version


def file_set(directory, names):
    p._real_path(directory)
    require(directory.is_dir())
    found = set()
    with os.scandir(directory) as entries:
        for entry in entries:
            require(len(found) < len(names) and entry.name not in found)
            found.add(entry.name)
    require(found == set(names))


def git_line(args):
    result = subprocess.run(["/usr/bin/git", *args], cwd=ROOT, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True)
    require(len(result.stdout) <= 4096)
    return result.stdout.decode("utf-8").strip()


def context(environment, phase):
    require(environment.get("PUBLISH_LINUX_ENGINEERING_CANDIDATE") == "true")
    require(environment.get("GITHUB_EVENT_NAME") == "workflow_dispatch"
            and environment.get("GITHUB_RUN_ATTEMPT") == "1"
            and environment.get("GITHUB_REF") == "refs/heads/main"
            and environment.get("GITHUB_REPOSITORY") == REPO)
    architecture = environment.get("QUALIFICATION_ARCHITECTURE")
    expected, run = environment.get("EXPECTED_SOURCE_COMMIT", ""), environment.get("GITHUB_RUN_ID", "")
    require(architecture in q.LANES and re.fullmatch(r"[0-9a-f]{40}", expected) is not None
            and environment.get("GITHUB_SHA") == expected and re.fullmatch(r"[0-9]{1,20}", run) is not None)
    require(environment.get("RUSTUP_TOOLCHAIN") == "1.99.1-" + q.LANES[architecture][1])
    require(sys.platform == "linux" and environment.get("RUNNER_OS") == "Linux"
            and environment.get("RUNNER_ARCH") == ("ARM64" if architecture == "arm64" else "X64"))
    temporary = Path(environment["RUNNER_TEMP"])
    p._real_path(temporary)
    parent = temporary / ("migration-stream-linux-candidate-" + architecture + "-" + run)
    if phase == "--stage":
        p._real_path(parent, absent=True)
        parent.mkdir(mode=0o700)
    p._real_path(parent)
    require(git_line(["rev-parse", "HEAD"]) == expected)
    require(git_line(["status", "--porcelain=v1", "--untracked-files=all"]) == "")
    require(git_line(["rev-parse", "refs/remotes/origin/main"]) == expected)
    tree = git_line(["rev-parse", "HEAD^{tree}"])
    require(re.fullmatch(r"[0-9a-f]{40}", tree) is not None)
    return {"runId": run, "architecture": architecture, "target": q.LANES[architecture][1],
            "sourceCommit": expected, "sourceTree": tree, "parent": parent, "temporary": temporary}


def validate(qualification, receipt, ctx, records):
    require(type(qualification) is dict and type(receipt) is dict)
    require(qualification.get("schemaVersion") == "migration-stream/linux-package-qualification/v1"
            and qualification.get("status") == "QUALIFIED_OFFLINE_PACKAGE_PAYLOAD"
            and qualification.get("sourceCommit") == ctx["sourceCommit"]
            and qualification.get("sourceTree") == ctx["sourceTree"]
            and qualification.get("architecture") == ctx["architecture"]
            and qualification.get("target") == ctx["target"]
            and qualification.get("runId") == ctx["runId"] and qualification.get("runAttempt") == 1
            and qualification.get("ubuntuSeries") == "24.04")
    for key in ("osInstallation", "appBrowserRegistration", "binaryUpload"):
        require(qualification.get(key) is False)
    require(type(qualification.get("productMetadataVerifyCalls")) is int
            and qualification["productMetadataVerifyCalls"] == 6)
    names, version = names_from(qualification)
    package_manifest = q.tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    require(package_manifest.get("name") == "migration-stream" and package_manifest.get("version") == version)
    deb_name, tar_name = names[0], names[1]
    require(set(records) == set(names))
    require(qualification.get("artifacts") == {name: records[name] for name in (deb_name, tar_name)})
    debian_version = version + "+git" + ctx["sourceCommit"][:12] + "-1"
    runtime = receipt.get("runtime")
    require(receipt.get("schemaVersion") == "migration-stream/linux-desktop-package/v1"
            and receipt.get("engineeringPolicy") == p.POLICY and receipt.get("package") == "migration-stream"
            and receipt.get("version") == debian_version
            and receipt.get("architecture") == q.LANES[ctx["architecture"]][2]
            and receipt.get("nativeUbuntuSeries") == "24.04" and receipt.get("productionVerifyCalls") == 2
            and receipt.get("supportedPlatformClaim") is False
            and receipt.get("artifacts") == qualification["artifacts"])
    require(type(runtime) is dict and runtime.get("productCommit") == ctx["sourceCommit"]
            and runtime.get("productVersion") == version and runtime.get("sourceState") == "CLEAN"
            and runtime.get("sourceSnapshotSha256") == qualification.get("sourceSnapshotSha256")
            and runtime.get("manifestSha256") == qualification.get("runtimeManifestSha256")
            and runtime.get("target") == {"os": "linux", "architecture": ctx["architecture"],
                                           "targetTriple": ctx["target"]})
    graph = receipt.get("runtimeFileGraph")
    require(type(graph) is dict and 0 < len(graph) <= 4096)
    return names


def sources(work, names):
    packages = work / "packages"
    return {name: work / "qualification.json" if name == "qualification.json" else packages / name
            for name in names}


def current_sources(work, ctx):
    p._real_path(work)
    require(work.parent == ctx["temporary"]
            and re.fullmatch(r"migration-stream-linux-qualification-[A-Za-z0-9_]{6,32}", work.name) is not None)
    qualification, qualification_row = read_json(work / "qualification.json", MAX_JSON)
    names, _version = names_from(qualification)
    paths = sources(work, names)
    records = {name: record(path, q.MAX_ARCHIVE if name not in ("package-receipt.json", "qualification.json") else MAX_JSON)
               for name, path in paths.items()}
    require(records["qualification.json"] == {"bytes": qualification_row["bytes"], "sha256": qualification_row["sha256"]})
    receipt, _receipt_row = read_json(paths["package-receipt.json"], MAX_JSON)
    validate(qualification, receipt, ctx, records)
    q.artifact_state(p, work / "packages", ("package-receipt.json", names[0], names[1]))
    require({name: record(path, q.MAX_ARCHIVE if name not in ("package-receipt.json", "qualification.json") else MAX_JSON)
             for name, path in paths.items()} == records)
    return records, names


def stage(work, ctx, qualification_sha, *, publisher=p._publish):
    require(digest(qualification_sha))
    records, names = current_sources(work, ctx)
    require(records["qualification.json"]["sha256"] == qualification_sha)
    destination = ctx["parent"] / "candidate"
    p._real_path(destination, absent=True)
    staging = Path(tempfile.mkdtemp(prefix=".candidate-", dir=destination.parent))
    paths = sources(work, names)
    for name in names:
        limit = q.MAX_ARCHIVE if name not in ("package-receipt.json", "qualification.json") else MAX_JSON
        copy_record(paths[name], staging / name, records[name], limit)
    require(current_sources(work, ctx)[0] == records)
    file_set(staging, names)
    publisher(staging, destination)
    file_set(destination, names)
    require({name: record(destination / name, q.MAX_ARCHIVE if name not in ("package-receipt.json", "qualification.json") else MAX_JSON)
             for name in names} == records)
    return {"schemaVersion": SCHEMA, "runId": ctx["runId"], "architecture": ctx["architecture"],
            "target": ctx["target"], "sourceCommit": ctx["sourceCommit"], "sourceTree": ctx["sourceTree"],
            "qualifiedWork": str(work), "files": records}


def artifact_name(ctx):
    return "migration-stream-linux-engineering-candidate-" + ctx["architecture"] + "-" + ctx["runId"]


def artifact_admission(inventory, ctx, identifier, sha, now):
    require(re.fullmatch(r"[0-9]{1,20}", identifier) is not None and digest(sha))
    require(type(inventory) is dict and type(inventory.get("total_count")) is int
            and inventory["total_count"] in (1, 2) and type(inventory.get("artifacts")) is list
            and len(inventory["artifacts"]) == inventory["total_count"])
    selected = [item for item in inventory["artifacts"] if type(item) is dict and item.get("id") == int(identifier)]
    require(len(selected) == 1 and len({item.get("id") for item in inventory["artifacts"]}) == len(inventory["artifacts"])
            and len({item.get("name") for item in inventory["artifacts"]}) == len(inventory["artifacts"]))
    allowed = {"migration-stream-linux-engineering-candidate-" + architecture + "-" + ctx["runId"]
               for architecture in q.LANES}
    require(all(type(item) is dict and item.get("name") in allowed for item in inventory["artifacts"]))
    item = selected[0]
    require(item.get("name") == artifact_name(ctx) and item.get("digest") == "sha256:" + sha
            and item.get("expired") is False and type(item.get("size_in_bytes")) is int
            and 0 < item["size_in_bytes"] <= 2 * q.MAX_ARCHIVE + 4 * MAX_JSON)
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
            and authority.get("architecture") == ctx["architecture"] and authority.get("target") == ctx["target"]
            and authority.get("sourceCommit") == ctx["sourceCommit"] and authority.get("sourceTree") == ctx["sourceTree"])
    records, names = current_sources(Path(authority["qualifiedWork"]), ctx)
    require(records == authority.get("files") and set(records) == set(names))
    for directory in (ctx["parent"] / "candidate", download):
        file_set(directory, names)
        require({name: record(directory / name, q.MAX_ARCHIVE if name not in ("package-receipt.json", "qualification.json") else MAX_JSON)
                 for name in names} == records)
        qualification, _row = read_json(directory / "qualification.json", MAX_JSON)
        receipt, _receipt_row = read_json(directory / "package-receipt.json", MAX_JSON)
        validate(qualification, receipt, ctx, records)
    require(current_sources(Path(authority["qualifiedWork"]), ctx)[0] == records)
    return {"schemaVersion": SCHEMA, "status": "LINUX_CANDIDATE_PUBLISHED_PASSIVE_REPLAY", "runId": ctx["runId"],
            "architecture": ctx["architecture"], "target": ctx["target"], "sourceCommit": ctx["sourceCommit"],
            "sourceTree": ctx["sourceTree"], "files": records, "artifact": artifact, "retentionDays": 7,
            "uploaded": True, "osInstallation": False, "appBrowserRegistration": False,
            "packageInstalled": False, "packageExecuted": False}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        raise ValueError("Linux candidate metadata redirect refused")


def metadata_get(opener, token, run, clock=time.monotonic):
    require(type(token) is str and 0 < len(token) <= 4096 and "\n" not in token and "\r" not in token)
    require(re.fullmatch(r"[0-9]{1,20}", run) is not None)
    request = urllib.request.Request(
        "https://api.github.com/repos/" + REPO + "/actions/runs/" + run + "/artifacts?per_page=2",
        headers={"Authorization": "Bearer " + token, "Accept": "application/vnd.github+json",
                 "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "migration-stream-linux-candidate"})
    deadline = clock() + 15
    with opener.open(request, timeout=10) as response:
        require(type(response.status) is int and response.status == 200)
        raw = response.read(MAX_JSON + 1)
        require(len(raw) <= MAX_JSON and clock() < deadline)
    return p._json(raw)


def outputs(values):
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
        for key, value in values.items():
            require(re.fullmatch(r"[a-z_]+", key) is not None and re.fullmatch(r"[A-Za-z0-9._-]{1,128}", str(value)) is not None)
            stream.write(key + "=" + str(value) + "\n")


def trusted_json(path, sha):
    value, row = read_json(path, MAX_JSON)
    require(digest(sha) and row["sha256"] == sha)
    return value


def write_json(path, value):
    body = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    require(len(body) <= MAX_JSON)
    descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
    try:
        os.write(descriptor, body)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def main():
    try:
        require(sys.argv[1:] in (["--stage"], ["--admit-artifact"], ["--replay"]))
        phase = sys.argv[1]
        ctx = context(dict(os.environ), phase)
        parent = ctx["parent"]
        if phase == "--stage":
            authority = stage(Path(os.environ["QUALIFIED_WORKSPACE"]), ctx, os.environ.get("QUALIFICATION_SHA"))
            path = parent / "candidate-authority.json"
            write_json(path, authority)
            outputs({"candidate_ready": "true", "authority_sha": record(path, MAX_JSON)["sha256"]})
            print('{"status":"LINUX_CANDIDATE_STAGED","uploaded":false,"packageInstalled":false}')
        else:
            identifier, sha = os.environ["CANDIDATE_ARTIFACT_ID"], os.environ["CANDIDATE_ARTIFACT_DIGEST"]
            if phase == "--admit-artifact":
                inventory = metadata_get(urllib.request.build_opener(NoRedirect), os.environ["GH_TOKEN"], ctx["runId"])
                admitted = artifact_admission(inventory, ctx, identifier, sha, dt.datetime.now(dt.timezone.utc))
                path = parent / "candidate-artifact.json"
                write_json(path, {"inventory": inventory, "admitted": admitted})
                outputs({"artifact_metadata_sha": record(path, MAX_JSON)["sha256"]})
            else:
                metadata = trusted_json(parent / "candidate-artifact.json", os.environ.get("CANDIDATE_METADATA_SHA"))
                admitted = artifact_admission(metadata["inventory"], ctx, identifier, sha, dt.datetime.now(dt.timezone.utc))
                require(admitted == metadata["admitted"])
                authority = trusted_json(parent / "candidate-authority.json", os.environ.get("CANDIDATE_AUTHORITY_SHA"))
                result = replay(parent / "download", authority, admitted, ctx)
                write_json(parent / "publication.json", result)
                print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except Exception:
        print('{"status":"LINUX_CANDIDATE_REFUSED"}')
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
