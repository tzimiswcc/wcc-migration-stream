#!/usr/bin/env python3
"""Create one unsigned engineering layout from already-built trusted artifacts.

This assembles bytes; it does not build, sign, install, register or qualify them.
The caller owns wrapper/runtime provenance. No public validation bypass exists.
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import plistlib
import re
import selectors
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path


POLICY = "local-unsigned-engineering"
CLI_SCHEMA = "migration-stream/assisted-local-runtime/cli/v1"
REPORT_KEYS = {
    "bundleRoot", "manifestSha256", "productCommit", "productVersion", "sourceState",
    "sourceSnapshotSha256", "target", "launcherSha256", "runtimeControllerSha256",
    "integrityCliSha256", "workbenchSha256", "memberCount", "totalMemberBytes",
}
HASH_KEYS = {"manifestSha256", "sourceSnapshotSha256", "launcherSha256", "runtimeControllerSha256", "integrityCliSha256", "workbenchSha256"}
MAX_FILE = 128 * 1024 * 1024
MAX_TOTAL = 513 * 1024 * 1024


class AssemblyError(ValueError):
    pass


@dataclass(frozen=True)
class Request:
    wrapper: Path
    runtime: Path
    destination: Path
    bundle_identifier: str
    minimum_os: str
    bundle_version: str
    short_version: str
    engineering_policy: str


@dataclass(frozen=True)
class WrapperInfo:
    architecture: str
    minimum_os: tuple[int, int, int]


def _version(text: str) -> tuple[int, int, int]:
    if not isinstance(text, str) or not re.fullmatch(r"[0-9]{1,5}\.[0-9]{1,3}(?:\.[0-9]{1,3})?", text):
        raise AssemblyError("minimum OS must be an explicit numeric major.minor[.patch]")
    parts = tuple(int(part) for part in text.split("."))
    if parts[0] == 0:
        raise AssemblyError("minimum OS major version must be positive")
    return (parts + (0,))[:3]


def _signature(info):
    return (info.st_dev, info.st_ino, info.st_size, stat.S_IMODE(info.st_mode), info.st_mtime_ns, info.st_ctime_ns)


def _real_path(path: Path, *, absent=False):
    if not path.is_absolute() or ".." in path.parts:
        raise AssemblyError("artifact paths must be absolute without parent traversal")
    for part in [*reversed(path.parents), path]:
        try:
            info = part.lstat()
        except FileNotFoundError:
            if absent and part == path:
                return
            raise AssemblyError("artifact or output parent is missing") from None
        if stat.S_ISLNK(info.st_mode):
            raise AssemblyError("linked path components are not accepted")
        if part != path and not stat.S_ISDIR(info.st_mode):
            raise AssemblyError("path parent is not a real directory")
    if absent:
        raise AssemblyError("destination already exists; create-only assembly never replaces it")


def _file_record(path: Path):
    named = path.lstat()
    if not stat.S_ISREG(named.st_mode):
        raise AssemblyError("artifact is not a regular file")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > MAX_FILE:
            raise AssemblyError("artifact is not one bounded unaliased regular file")
        digest = hashlib.sha256()
        count = 0
        while data := os.read(descriptor, 1024 * 1024):
            count += len(data)
            if count > before.st_size:
                raise AssemblyError("artifact changed during capture")
            digest.update(data)
        if count != before.st_size or _signature(before) != _signature(os.fstat(descriptor)) or _signature(before) != _signature(path.lstat()):
            raise AssemblyError("artifact identity changed during capture")
        return ("file", stat.S_IMODE(before.st_mode), count, digest.hexdigest(), _signature(before))
    finally:
        os.close(descriptor)


def _snapshot(root: Path):
    entries = {}
    total = 0
    admitted = 1  # The root consumes one global entry before any descent.
    def walk(path):
        nonlocal total, admitted
        if len(path.relative_to(root).parts) > 64:
            raise AssemblyError("runtime path depth exceeds assembly bound")
        info = path.lstat()
        relative = path.relative_to(root).as_posix()
        if stat.S_ISDIR(info.st_mode):
            entries[relative] = ("directory", stat.S_IMODE(info.st_mode), 0, "", _signature(info))
            children = []
            with os.scandir(path) as iterator:
                for child in iterator:
                    if admitted >= 4096:
                        raise AssemblyError("runtime graph exceeds assembly bounds")
                    admitted += 1
                    children.append(Path(child.path))
            # Capture is globally bounded before deterministic ordering/descent.
            for child in sorted(children):
                walk(child)
        elif stat.S_ISREG(info.st_mode):
            entries[relative] = _file_record(path)
            total += entries[relative][2]
        else:
            raise AssemblyError("runtime contains linked or special entries")
        if total > MAX_TOTAL:
            raise AssemblyError("runtime graph exceeds assembly bounds")
    walk(root)
    return entries


def _contents(snapshot):
    return {name: record[:4] for name, record in snapshot.items()}


def _copy_file(source: Path, destination: Path, expected):
    source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    destination_fd = None
    try:
        if _signature(os.fstat(source_fd)) != expected[4]:
            raise AssemblyError("source changed before copying")
        destination_fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(destination_fd, "wb", closefd=False) as output:
            count = 0
            while data := os.read(source_fd, 1024 * 1024):
                count += len(data)
                if count > expected[2]:
                    raise AssemblyError("source grew during copying")
                output.write(data)
            output.flush()
        if _signature(os.fstat(source_fd)) != expected[4] or _signature(source.lstat()) != expected[4]:
            raise AssemblyError("source changed during copying")
        os.fchmod(destination_fd, expected[1])
        os.fsync(destination_fd)
    finally:
        os.close(source_fd)
        if destination_fd is not None:
            os.close(destination_fd)
    if _file_record(destination)[:4] != expected[:4]:
        raise AssemblyError("copied artifact bytes or mode differ")


def _copy_runtime(source, destination, snapshot):
    for name, record in snapshot.items():
        target = destination if name == "." else destination / name
        if record[0] == "directory":
            target.mkdir(mode=0o700)
        else:
            _copy_file(source / name, target, record)
    for name, record in reversed(list(snapshot.items())):
        if record[0] == "directory":
            target = destination if name == "." else destination / name
            target.chmod(record[1])
            descriptor = os.open(target, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)


def _inspect_wrapper(path: Path, expected_identity=None) -> WrapperInfo:
    # Inspect only bounded existing Mach-O bytes, never invoke file/otool/compiler.
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as artifact:
        info = os.fstat(artifact.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > MAX_FILE:
            raise AssemblyError("wrapper is not a bounded unaliased regular file")
        if expected_identity is not None and _signature(info) != expected_identity:
            raise AssemblyError("wrapper identity changed before deployment inspection")
        header = artifact.read(32)
        if len(header) != 32:
            raise AssemblyError("wrapper lacks a Mach-O header")
        magic, cpu, subtype, kind, count, size, _, _ = struct.unpack("<8I", header)
        if (magic, cpu, subtype, kind) != (0xFEEDFACF, 0x0100000C, 0, 2):
            raise AssemblyError("wrapper must be one thin arm64 MH_EXECUTE artifact")
        if not 1 <= count <= 4096 or not 8 <= size <= 1024 * 1024:
            raise AssemblyError("wrapper load-command bounds are invalid")
        commands = artifact.read(size)
        if len(commands) != size:
            raise AssemblyError("wrapper load commands are truncated")
        if _signature(info) != _signature(os.fstat(artifact.fileno())) or _signature(info) != _signature(path.lstat()):
            raise AssemblyError("wrapper changed during deployment inspection")
    offset, minimums = 0, []
    for _ in range(count):
        if offset + 8 > size:
            raise AssemblyError("wrapper load-command count is inconsistent")
        command, length = struct.unpack_from("<2I", commands, offset)
        if length < 8 or length % 8 or offset + length > size:
            raise AssemblyError("wrapper load-command extent is invalid")
        if command == 0x32:  # LC_BUILD_VERSION
            if length < 24:
                raise AssemblyError("wrapper build-version command is truncated")
            platform, minimum, _, tools = struct.unpack_from("<4I", commands, offset + 8)
            if platform != 1 or length != 24 + tools * 8:
                raise AssemblyError("wrapper platform/build-version evidence is invalid")
            minimums.append(minimum)
        elif command == 0x24:  # LC_VERSION_MIN_MACOSX
            if length != 16:
                raise AssemblyError("wrapper minimum-version command is invalid")
            minimums.append(struct.unpack_from("<I", commands, offset + 8)[0])
        elif command in (0x25, 0x2F, 0x30):
            raise AssemblyError("wrapper contains a different platform deployment command")
        offset += length
    if offset != size or len(minimums) != 1 or minimums[0] < 0x10000:
        raise AssemblyError("wrapper needs exactly one unambiguous macOS deployment command")
    value = minimums[0]
    return WrapperInfo("arm64", (value >> 16, (value >> 8) & 255, value & 255))


def _verify_runtime(root: Path):
    if sys.platform != "darwin":
        raise AssemblyError("real runtime verification requires the matching qualified macOS host")
    # Trusted input only. This is the existing fixed controller's real verify contract.
    process = subprocess.Popen([str(root / "libexec/migration-stream-local-runtime"), "verify"],
                               stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               env={}, shell=False)
    output, errors = bytearray(), bytearray()
    selector = selectors.DefaultSelector()
    try:
        for stream, buffer in ((process.stdout, output), (process.stderr, errors)):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, buffer)
        deadline = time.monotonic() + 120
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssemblyError("trusted runtime verify timed out")
            events = selector.select(min(0.1, remaining))
            if not events and process.poll() is not None:
                break  # Never wait for inherited pipe EOF from descendants.
            for key, _ in events:
                data = os.read(key.fileobj.fileno(), 4096)
                if not data:
                    selector.unregister(key.fileobj)
                else:
                    if len(data) > 65_536 - len(key.data):
                        raise AssemblyError("trusted runtime verify output exceeds bound")
                    key.data.extend(data)
        try:
            status = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            raise AssemblyError("trusted runtime verify timed out") from None
        if status != 0:
            raise AssemblyError("trusted runtime controller verification failed")
        return json.loads(output)
    finally:
        selector.close()
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    pass
        process.stdout.close()
        process.stderr.close()


def _report(envelope, root):
    if not isinstance(envelope, dict) or envelope.get("schemaVersion") != CLI_SCHEMA or envelope.get("ok") is not True:
        raise AssemblyError("runtime did not return the existing successful verify envelope")
    result = envelope.get("result")
    if not isinstance(result, dict) or set(result) != REPORT_KEYS or result["bundleRoot"] != str(root):
        raise AssemblyError("runtime verify result fields/root differ from actual contract")
    target = result["target"]
    if not isinstance(target, dict) or set(target) != {"os", "architecture", "targetTriple", "minimumOsVersion"} or (target["os"], target["architecture"], target["targetTriple"]) != ("macos", "arm64", "aarch64-apple-darwin"):
        raise AssemblyError("runtime target is not the initial thin Apple-silicon lane")
    _version(target["minimumOsVersion"])
    if any(not isinstance(result[key], str) or not re.fullmatch("[0-9a-f]{64}", result[key]) for key in HASH_KEYS):
        raise AssemblyError("runtime verify hash identity is malformed")
    if result["sourceState"] not in ("CLEAN", "EXPLICIT_UNCOMMITTED_SNAPSHOT") or any(not isinstance(result[key], str) or not result[key] or len(result[key]) > 128 for key in ("productCommit", "productVersion")):
        raise AssemblyError("runtime product/source identity is malformed")
    if any(type(result[key]) is not int or result[key] <= 0 for key in ("memberCount", "totalMemberBytes")):
        raise AssemblyError("runtime verify counts are malformed")
    return {key: value for key, value in result.items() if key != "bundleRoot"}


def _write_plist(path, values):
    data = plistlib.dumps(values, fmt=plistlib.FMT_XML, sort_keys=True)
    with path.open("xb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    path.chmod(0o444)


def _publish_noreplace(stage: Path, destination: Path):
    if stage.parent != destination.parent:
        raise AssemblyError("atomic publication requires sibling staging and destination")
    expected = stage.lstat()
    if not stat.S_ISDIR(expected.st_mode):
        raise AssemblyError("publication staging is not a real directory")
    if sys.platform != "darwin":
        raise AssemblyError("real engineering publication requires Darwin renameatx_np RENAME_EXCL")
    library = ctypes.CDLL(None, use_errno=True)
    rename = getattr(library, "renameatx_np", None)
    if rename is None:
        raise AssemblyError("native atomic no-replace publication is unavailable")
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    parent = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        named = os.stat(stage.name, dir_fd=parent, follow_symlinks=False)
        if (named.st_dev, named.st_ino) != (expected.st_dev, expected.st_ino):
            raise AssemblyError("staging identity changed before publication")
        if rename(parent, os.fsencode(stage.name), parent, os.fsencode(destination.name), 0x4) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error), destination)
        os.fsync(parent)
    finally:
        os.close(parent)


def _cleanup_owned(stage, identity):
    try:
        current = stage.lstat()
    except FileNotFoundError:
        return
    if (current.st_dev, current.st_ino) != identity or not stat.S_ISDIR(current.st_mode):
        raise AssemblyError("owned staging identity changed; refusing cleanup")
    for directory, _, _ in os.walk(stage, followlinks=False):
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fchmod(descriptor, stat.S_IMODE(os.fstat(descriptor).st_mode) | stat.S_IWUSR)
        finally:
            os.close(descriptor)
    shutil.rmtree(stage)


def assemble(request: Request, *, validator=_verify_runtime, wrapper_inspector=_inspect_wrapper,
             publisher=_publish_noreplace):
    # Injection is an internal callable test seam, never a public bypass option.
    if request.engineering_policy != POLICY:
        raise AssemblyError("explicit local-unsigned-engineering policy is required")
    if len(request.bundle_identifier) > 255 or not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+", request.bundle_identifier):
        raise AssemblyError("an explicit bundle identifier is required")
    if len(request.bundle_version) > 64 or len(request.short_version) > 64 or not re.fullmatch(r"[0-9]+(?:\.[0-9]+){0,2}", request.bundle_version) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", request.short_version):
        raise AssemblyError("explicit numeric bundle and short versions are required")
    minimum = _version(request.minimum_os)
    for path in (request.wrapper, request.runtime):
        _real_path(path)
    _real_path(request.destination, absent=True)
    for first, second in ((request.wrapper, request.runtime), (request.wrapper, request.destination), (request.runtime, request.destination)):
        if first == second or first in second.parents or second in first.parents:
            raise AssemblyError("wrapper/runtime/output paths must not overlap")
    if not stat.S_ISDIR(request.runtime.lstat().st_mode):
        raise AssemblyError("runtime root is not a real directory")
    wrapper = _file_record(request.wrapper)
    if not wrapper[1] & stat.S_IXUSR:
        raise AssemblyError("already-built wrapper is not executable")
    wrapper_info = wrapper_inspector(request.wrapper, wrapper[4])
    if wrapper_info.architecture != "arm64":
        raise AssemblyError("wrapper architecture differs from runtime lane")
    original = _snapshot(request.runtime)
    source = _report(validator(request.runtime), request.runtime)
    if minimum < max(wrapper_info.minimum_os, _version(source["target"]["minimumOsVersion"])):
        raise AssemblyError("operator minimum OS understates known wrapper/runtime minimum")
    if _snapshot(request.runtime) != original or _file_record(request.wrapper) != wrapper:
        raise AssemblyError("trusted source changed during admission")
    stage = Path(tempfile.mkdtemp(prefix=".migration-stream-engineering-", dir=request.destination.parent))
    identity = (stage.stat().st_dev, stage.stat().st_ino)
    try:
        contents = stage / "Contents"
        macos, resources = contents / "MacOS", contents / "Resources"
        for directory in (contents, macos, resources):
            directory.mkdir(mode=0o700)
        _copy_file(request.wrapper, macos / "MigrationStream", wrapper)
        staged_runtime = resources / "Runtime"
        _copy_runtime(request.runtime, staged_runtime, original)
        _write_plist(contents / "Info.plist", {
            "CFBundleExecutable": "MigrationStream", "CFBundleIdentifier": request.bundle_identifier,
            "CFBundleName": "Migration Stream", "CFBundlePackageType": "APPL",
            "CFBundleVersion": request.bundle_version, "CFBundleShortVersionString": request.short_version,
            "LSMinimumSystemVersion": request.minimum_os,
        })
        staged = _report(validator(staged_runtime), staged_runtime)
        if staged != source or _contents(_snapshot(staged_runtime)) != _contents(original):
            raise AssemblyError("staged runtime verify identity/bytes/modes differ from source")
        if _snapshot(request.runtime) != original or _file_record(request.wrapper) != wrapper:
            raise AssemblyError("trusted source changed during staging")
        for directory in (macos, resources, contents, stage):
            directory.chmod(0o755)
            descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        publisher(stage, request.destination)
        return request.destination
    finally:
        _cleanup_owned(stage, identity)


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("wrapper", "runtime", "destination"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("bundle-identifier", "minimum-os", "bundle-version", "short-version"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--engineering-policy", required=True, choices=[POLICY])
    return Request(**vars(parser.parse_args(argv)))


def main():
    try:
        destination = assemble(arguments())
        print(f"Created local unsigned engineering layout: {destination}")
    except (AssemblyError, OSError, ValueError, TypeError) as error:
        print(f"Engineering assembly refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
