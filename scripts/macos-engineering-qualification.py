#!/usr/bin/env python3
"""One manual ARM64 engineering build and owned, read-only DMG qualification.

No application launch, installation, signing, upload, force-detach or cleanup
crawl. Failed and successful private workspaces are retained for inspection.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import selectors
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import tomllib

SPEC = importlib.util.spec_from_file_location(
    "macos_desktop_package", Path(__file__).with_name("package-macos-engineering-desktop.py"))
p = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(p)
a = p.assembler
SCHEMA = "migration-stream/macos-engineering-qualification/v1"
HDIUTIL = "/usr/bin/hdiutil"
MAX_PLIST = 1024 * 1024
MAX_LOG = 8 * 1024 * 1024
WHOLE_SECONDS = 3600


class QualificationError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise QualificationError(message)


def write_json(path, value):
    data = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    require(len(data) <= MAX_PLIST, "receipt exceeds bound")
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)  # Persist the new journal name before a mount can start.
    finally:
        os.close(descriptor)


def read_json(path):
    a._real_path(path)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        before = os.fstat(descriptor)
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1
                and 0 < before.st_size <= MAX_PLIST, "receipt file refused")
        data = bytearray()
        while chunk := os.read(descriptor, min(65536, MAX_PLIST + 1 - len(data))):
            data.extend(chunk)
            require(len(data) <= MAX_PLIST, "receipt exceeds bound")
        require(len(data) == before.st_size and a._signature(before) == a._signature(os.fstat(descriptor))
                and a._signature(before) == a._signature(path.lstat()), "receipt changed during read")
        def pairs(items):
            result = {}
            for key, value in items:
                require(key not in result, "duplicate receipt key")
                result[key] = value
            return result
        return json.loads(data, object_pairs_hook=pairs)
    finally:
        os.close(descriptor)


def directory_id(path):
    a._real_path(path)
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode), "real directory required")
    return info.st_dev, info.st_ino


class Runner:
    """Bounded direct-child runner; retained logs, no shell or inherited stdin.

    It is not a descendant sandbox. A tool may use Apple/compiler subprocesses;
    an unresolved direct child or attachment prevents qualification.
    """
    def __init__(self, root, *, popen=None, clock=time.monotonic):
        self.root, self.popen, self.clock = root, subprocess.Popen if popen is None else popen, clock
        self.deadline = clock() + WHOLE_SECONDS
        self.sequence = 0
        self.cleanup_unresolved = False

    def checkpoint(self):
        require(self.clock() < self.deadline, "whole qualification deadline exceeded")

    def __call__(self, arguments, *, seconds=60, limit=65536, env=None, cwd=None):
        self.checkpoint()
        require(type(limit) is int and 0 < limit <= MAX_LOG, "invalid output bound")
        self.sequence += 1
        label = f"child-{self.sequence:02d}"
        deadline = min(self.deadline, self.clock() + seconds)
        output, errors = bytearray(), bytearray()
        selector = selectors.DefaultSelector()  # No child exists if selector construction fails.
        child = None
        try:
            child = self.popen([str(item) for item in arguments], stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False,
                               env={} if env is None else env, cwd=cwd)
            for stream, buffer in ((child.stdout, output), (child.stderr, errors)):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, buffer)
            while selector.get_map():
                require(self.clock() < deadline, "child deadline exceeded")
                events = selector.select(min(0.1, max(0, deadline - self.clock())))
                for key, _ in events:
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    require(self.clock() < deadline, "child deadline exceeded")
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        require(len(key.data) + len(chunk) <= limit, "child output exceeds bound")
                        key.data.extend(chunk)
                if not events and child.poll() is not None:
                    break  # Descendant-held pipes are not an unbounded EOF wait.
            require(self.clock() < deadline, "child deadline exceeded")
            status = child.wait(timeout=max(0.001, deadline - self.clock()))
            require(self.clock() < deadline, "child deadline exceeded")
            require(status == 0, "child refused qualification phase")
            return bytes(output)
        except subprocess.TimeoutExpired:
            raise QualificationError("child deadline exceeded") from None
        finally:
            selector.close()
            if child is not None and child.poll() is None:
                try:
                    child.terminate()
                except OSError:
                    self.cleanup_unresolved = True
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    try:
                        child.kill()
                    except OSError:
                        self.cleanup_unresolved = True
                    try:
                        child.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        self.cleanup_unresolved = True
            if child is not None:
                for stream in (child.stdout, child.stderr):
                    try:
                        stream.close()
                    except OSError:
                        self.cleanup_unresolved = True
            for suffix, data in (("stdout", output), ("stderr", errors)):
                with (self.root / f"{label}.{suffix}.log").open("xb") as stream:
                    stream.write(data)


def parse_plist(data):
    require(type(data) is bytes and 0 < len(data) <= MAX_PLIST, "image metadata exceeds bound")
    # Only the XML plist emitted by the fixed hdiutil command is admitted.
    require(data.lstrip().startswith(b"<?xml"), "XML image metadata required")
    value = plistlib.loads(data)
    require(type(value) is dict, "image metadata object required")
    return value


def entities(value, mount):
    rows = value.get("system-entities")
    require(type(rows) is list and 0 < len(rows) <= 32, "image device list refused")
    devices, wholes, mounted = [], [], []
    for row in rows:
        require(type(row) is dict, "image device row refused")
        device = row.get("dev-entry")
        require(type(device) is str and re.fullmatch(r"/dev/disk[0-9]{1,5}(?:s[0-9]{1,5})?", device),
                "image device name refused")
        require(device not in devices, "duplicate image device")
        devices.append(device)
        if re.fullmatch(r"/dev/disk[0-9]{1,5}", device):
            wholes.append(device)
        if "mount-point" in row:
            require(row["mount-point"] == str(mount), "unexpected mounted image volume")
            mounted.append(device)
    require(len(wholes) == 1 and len(mounted) == 1, "ambiguous image ownership")
    require(all(device == wholes[0] or device.startswith(wholes[0] + "s") for device in devices),
            "partition is not on reported whole device")
    return {"wholeDevice": wholes[0], "volumeDevice": mounted[0], "devices": sorted(devices)}


def image_rows(data, image):
    value = parse_plist(data)
    rows = value.get("images")
    require(type(rows) is list and len(rows) <= 128, "attached image inventory refused")
    # Do not log or expose unrelated host image paths/devices.
    return [row for row in rows if type(row) is dict and row.get("image-path") == str(image)]


def assert_ownership(data, image, mount, expected):
    rows = image_rows(data, image)
    require(len(rows) == 1 and entities(rows[0], mount) == expected,
            "fresh attached-image ownership differs")


def mounted_identity(mount, parent, expected_parent, *, readonly=None):
    require(directory_id(parent) == expected_parent, "private mount parent changed")
    identity = directory_id(mount)
    if readonly is None:
        readonly = bool(os.statvfs(mount).f_flag & os.ST_RDONLY)
    require(readonly, "mounted filesystem is not read-only")
    require(identity[0] != expected_parent[0], "mount did not replace empty private mountpoint")
    return identity


def graph(root, expected):
    current = a._snapshot(root)
    require(a._contents(current) == expected, "application graph, bytes or modes differ")
    return current


def qualify_image(package, work, receipt, runner, *, validator=None, mount_check=mounted_identity):
    """Private injection seam for synthetic state tests; CLI supplies real tools."""
    image, app = package / p.IMAGE_NAME, package / p.APP_NAME
    image_pin = p._image_record(image)
    require(image_pin == {key: receipt["image"][key] for key in ("bytes", "sha256")},
            "image differs from package receipt")
    expected = {name: tuple(row) for name, row in receipt["app"]["entries"].items()}
    original = graph(app, expected)
    parent = work / "attachment"
    parent.mkdir(mode=0o700)
    parent_pin = directory_id(parent)
    mount = parent / "volume"
    mount.mkdir(mode=0o700)
    empty_pin = directory_id(mount)
    require(not list(mount.iterdir()), "mountpoint is not empty")
    info_command = [HDIUTIL, "info", "-plist"]
    require(not image_rows(runner(info_command, limit=MAX_PLIST), image), "image already attached")
    require(p._image_record(image) == image_pin, "image changed before attach")
    # Persist BEFORE starting attach: failure/timeout need not mean no mount.
    write_json(work / "01-attach-pending.json", {"state": "ATTACH_PENDING", "image": image_pin})
    attach = runner([HDIUTIL, "attach", "-readonly", "-nobrowse", "-noautoopen", "-owners", "on",
                     "-mountpoint", str(mount), "-plist", str(image)], seconds=60, limit=MAX_PLIST)
    witness = entities(parse_plist(attach), mount)
    write_json(work / "02-attach-witness.json", {"state": "MOUNTED", **witness})
    verify = validator
    if verify is None:
        def verify(root):
            return json.loads(runner([root / "libexec/migration-stream-local-runtime", "verify"],
                                     seconds=120, limit=65536))
    failure = None
    mounted_pin = None
    result = None
    try:
        assert_ownership(runner(info_command, limit=MAX_PLIST), image, mount, witness)
        mounted_pin = mount_check(mount, parent, parent_pin)
        mounted_app = mount / p.APP_NAME
        mounted = graph(mounted_app, expected)
        runtime = mounted_app / "Contents/Resources/Runtime"
        require(a._report(verify(runtime), runtime) == receipt["runtime"], "mounted Runtime identity differs")
        require(a._snapshot(mounted_app) == mounted, "mounted application changed during verify")
        require(mount_check(mount, parent, parent_pin) == mounted_pin, "mounted root changed")
        assert_ownership(runner(info_command, limit=MAX_PLIST), image, mount, witness)
        copy = work / p.APP_NAME
        a._real_path(copy, absent=True)
        a._copy_runtime(mounted_app, copy, mounted)
        copied = graph(copy, expected)
        runtime = copy / "Contents/Resources/Runtime"
        require(a._report(verify(runtime), runtime) == receipt["runtime"], "private-copy Runtime identity differs")
        require(a._snapshot(copy) == copied and a._snapshot(mounted_app) == mounted,
                "application changed during private-copy verification")
        require(a._snapshot(app) == original and p._image_record(image) == image_pin,
                "package changed during mounted qualification")
        runner.checkpoint()
        result = {"image": image_pin, "appEntries": len(expected), "mountedRuntimeVerified": True,
                  "privateCopyRuntimeVerified": True, "mountedGraphAndModesEqual": True,
                  "privateCopyGraphAndModesEqual": True}
    except Exception as error:
        failure = error
    # Never infer a device after an uncertain attach. Only a complete returned
    # witness permits this one detach, after another exact fresh ownership check.
    try:
        require(directory_id(parent) == parent_pin, "private mount parent changed before detach")
        assert_ownership(runner(info_command, limit=MAX_PLIST), image, mount, witness)
        if mounted_pin is not None:
            require(mount_check(mount, parent, parent_pin) == mounted_pin, "mounted root changed before detach")
        require(p._image_record(image) == image_pin, "image changed before detach")
        write_json(work / "03-detach-pending.json", {"state": "DETACH_PENDING", **witness})
        runner([HDIUTIL, "detach", witness["wholeDevice"]], seconds=60, limit=65536)
        require(not image_rows(runner(info_command, limit=MAX_PLIST), image), "image remains attached")
        require(directory_id(parent) == parent_pin and directory_id(mount) == empty_pin
                and not list(mount.iterdir()), "original empty mountpoint did not return")
        write_json(work / "04-detached.json", {"state": "DETACHED", **witness})
    except Exception as error:
        raise QualificationError("owned image detach or final absence unresolved; retain workspace") from error
    if failure is not None:
        raise failure
    runner.checkpoint()
    return result


def fixed_environment(work):
    result = {key: os.environ[key] for key in ("PATH", "HOME", "RUSTUP_HOME", "CARGO_HOME") if key in os.environ}
    require("PATH" in result and "HOME" in result, "build environment missing")
    temporary = work / "temporary"
    temporary.mkdir(mode=0o700)
    result.update(TMPDIR=str(temporary), RUSTUP_TOOLCHAIN="1.99.1-aarch64-apple-darwin",
                  CARGO_TERM_COLOR="never", NO_COLOR="1", CI="true")
    return result


def tool_path(name):
    found = shutil.which(name)
    require(found is not None, "required installed tool missing")
    resolved = Path(found).resolve(strict=True)
    require(stat.S_ISREG(resolved.stat().st_mode) and os.access(resolved, os.X_OK), "installed tool is not executable")
    return Path(found)  # Preserve rustup proxy argv[0]; resolving rustc would invoke rustup itself.


def source_identity(source, expected, runner):
    actual = runner(["/usr/bin/git", "rev-parse", "HEAD"], cwd=source).decode().strip()
    require(actual == expected, "source commit differs")
    require(not runner(["/usr/bin/git", "status", "--porcelain", "--untracked-files=all"], cwd=source),
            "qualification requires clean source")
    return actual


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-source-commit", required=True)
    args = parser.parse_args(argv)
    work = None
    try:
        expected = args.expected_source_commit
        require(re.fullmatch(r"[0-9a-f]{40}", expected) is not None, "exact source commit required")
        require(sys.platform == "darwin" and platform.machine() == "arm64", "native Apple-silicon host required")
        require(platform.mac_ver()[0].split(".")[0] == "26", "qualification lane requires macOS 26")
        require(os.environ.get("GITHUB_EVENT_NAME") == "workflow_dispatch"
                and os.environ.get("GITHUB_RUN_ATTEMPT") == "1"
                and os.environ.get("GITHUB_SHA") == expected, "one exact manual workflow attempt required")
        source = Path(__file__).resolve().parents[1]
        parent = Path(os.environ.get("RUNNER_TEMP", ""))
        directory_id(parent)
        require(source != parent and source not in parent.parents and parent not in source.parents,
                "qualification and source roots overlap")
        work = Path(tempfile.mkdtemp(prefix="migration-stream-macos-qualification-", dir=parent))
        runner = Runner(work)
        env = fixed_environment(work)
        source_identity(source, expected, runner)
        node, npm, rustc = (tool_path(name) for name in ("node", "npm", "rustc"))
        require(runner([node, "--version"], env=env).decode().strip() == "v24.22.1", "Node version differs")
        require(runner([node, "-p", "process.arch"], env=env).decode().strip() == "arm64", "Node architecture differs")
        require(runner([npm, "--version"], env=env).decode().strip() == "12.2.1", "npm version differs")
        rust = runner([rustc, "-vV"], env=env).decode()
        require("release: 1.99.1\n" in rust and "host: aarch64-apple-darwin\n" in rust, "Rust version/host differs")
        swift = runner(["/usr/bin/xcrun", "--find", "swiftc"], env=env).decode().strip()
        require(Path(swift).is_absolute() and Path(swift).is_file(), "installed Swift compiler missing")
        swift_version = runner(["/usr/bin/xcrun", "swiftc", "--version"], env=env).decode()
        require(re.search(r"(?:Apple )?Swift version 6\.", swift_version) is not None
                and "Target: arm64-apple-macosx" in swift_version, "installed Swift 6 native witness differs")
        version = tomllib.loads((source / "Cargo.toml").read_text())["package"]["version"]
        require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None, "package version refused")
        # Populate the lockfile-controlled npm cache once; the unchanged builder
        # subsequently uses npm ci --offline. No application is started.
        runner([npm, "--prefix", source / "web", "ci", "--ignore-scripts", "--no-audit", "--no-fund"],
               seconds=600, limit=MAX_LOG, env=env, cwd=source)
        runtime = work / "migration-stream-local-macos-arm64"
        runner(["/bin/bash", source / "scripts/build-assisted-runtime-bundle.sh", "--output", runtime,
                "--target", "aarch64-apple-darwin", "--no-archive"],
               seconds=1800, limit=MAX_LOG, env=env, cwd=source)
        source_identity(source, expected, runner)
        wrapper = work / "MigrationStreamWrapper"
        runner(["/usr/bin/xcrun", "swiftc", "-swift-version", "6", "-warnings-as-errors", "-target",
                "arm64-apple-macos11", "-framework", "AppKit",
                source / "native/macos/DocumentOpenAdapter.swift",
                source / "native/macos/ApplicationBootstrap.swift",
                source / "native/macos/MigrationStreamApplication.swift", "-o", wrapper],
               seconds=180, limit=MAX_LOG, env=env, cwd=source)
        package = work / "package"
        runner([sys.executable, "-I", "-B", source / "scripts/package-macos-engineering-desktop.py",
                "--wrapper", wrapper, "--runtime", runtime, "--output-directory", package,
                "--bundle-identifier", "local.migrationstream.engineering", "--minimum-os", "11.0",
                "--bundle-version", "1", "--short-version", version,
                "--engineering-policy", a.POLICY], seconds=1200, limit=65536, env=env, cwd=source)
        receipt_path = package / p.RECEIPT_NAME
        require(receipt_path.stat().st_size <= MAX_PLIST, "package receipt exceeds bound")
        receipt_pin = a._file_record(receipt_path)
        receipt = read_json(receipt_path)
        require(receipt["schemaVersion"] == p.SCHEMA and receipt["runtime"]["productCommit"] == expected
                and receipt["runtime"]["productVersion"] == version and receipt["runtime"]["sourceState"] == "CLEAN",
                "package source identity differs")
        result = qualify_image(package, work, receipt, runner)
        require(a._file_record(receipt_path) == receipt_pin, "package receipt changed")
        source_identity(source, expected, runner)
        runner.checkpoint()
        require(not runner.cleanup_unresolved, "child cleanup unresolved")
        result.update(schemaVersion=SCHEMA, status="ENGINEERING_IMAGE_QUALIFIED", sourceCommit=expected,
                      target="aarch64-apple-darwin", qualificationHost="macos-26-arm64", packageReceiptSha256=receipt_pin[3],
                      wrapperSha256=a._file_record(wrapper)[3], installedSwiftVersion=swift_version.strip(),
                      nodeVersion="24.22.1", npmVersion="12.2.1", rustVersion="1.99.1",
                      packagingRuntimeVerifyCalls=4, mountedRuntimeVerifyCalls=1,
                      privateCopyRuntimeVerifyCalls=1, appLaunched=False, installed=False, signed=False,
                      notarized=False, uploaded=False)
        write_json(work / "qualification.json", result)
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        print("Retained runner-private qualification workspace: " + str(work))
        return 0
    except Exception:
        # Do not echo tool errors, environment/config, global image metadata or
        # arbitrary build output into the public job log. Bounded raw logs stay
        # in this disposable runner's private namespace and are never uploaded.
        print(json.dumps({"schemaVersion": SCHEMA, "status": "QUALIFICATION_REFUSED", "retained": work is not None}))
        if work is not None:
            print("Retained runner-private qualification workspace: " + str(work))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
