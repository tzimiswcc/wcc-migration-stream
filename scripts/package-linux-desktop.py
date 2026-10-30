#!/usr/bin/env python3
"""Create-only Ubuntu .deb and portable GNU/Linux tar.gz from a trusted Runtime.

No build, installation, association registration, service or product launch is
performed. The public CLI requires native production `verify` and dpkg-shlibdeps;
only private Python test seams replace these operations in synthetic tests.
"""
from __future__ import annotations

import argparse
import ctypes
from dataclasses import dataclass
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib


POLICY = "local-unsigned-engineering"
CLI_SCHEMA = "migration-stream/assisted-local-runtime/cli/v1"
REPORT_KEYS = {"bundleRoot", "manifestSha256", "productCommit", "productVersion", "sourceState",
               "sourceSnapshotSha256", "target", "launcherSha256", "runtimeControllerSha256",
               "integrityCliSha256", "workbenchSha256", "memberCount", "totalMemberBytes"}
HASH_KEYS = {"manifestSha256", "sourceSnapshotSha256", "launcherSha256",
             "runtimeControllerSha256", "integrityCliSha256", "workbenchSha256"}
ELF_PATHS = ("libexec/migration-stream-local-runtime", "bin/migration-stream",
             "bin/migration-stream-workbench")
TARGETS = {"x64": ("amd64", "x86_64-unknown-linux-gnu", 62, "x86_64"),
           "arm64": ("arm64", "aarch64-unknown-linux-gnu", 183, "aarch64")}
MAX_FILE = 128 * 1024 * 1024
MAX_TOTAL = 513 * 1024 * 1024
MAX_ARCHIVE = 1024 * 1024 * 1024
MAX_ENTRIES = 4096
MAX_TEXT = 1024 * 1024


class PackagingError(ValueError):
    pass


@dataclass(frozen=True)
class Request:
    runtime: Path
    destination: Path
    architecture: str
    expected_product_commit: str
    expected_source_snapshot_sha256: str
    expected_manifest_sha256: str
    maintainer: str
    revision: int
    engineering_policy: str


def _signature(info):
    return (info.st_dev, info.st_ino, info.st_size, stat.S_IMODE(info.st_mode),
            info.st_mtime_ns, info.st_ctime_ns)


def _real_path(path, *, absent=False):
    if not path.is_absolute() or ".." in path.parts:
        raise PackagingError("ordinary absolute artifact path required")
    for item in [*reversed(path.parents), path]:
        try:
            info = item.lstat()
        except FileNotFoundError:
            if absent and item == path:
                return
            raise PackagingError("artifact parent is missing") from None
        if stat.S_ISLNK(info.st_mode) or (item != path and not stat.S_ISDIR(info.st_mode)):
            raise PackagingError("linked or non-directory artifact ancestor")
    if absent:
        raise PackagingError("create-only destination already exists")


def _record(path, maximum=None):
    maximum = MAX_FILE if maximum is None else maximum
    named = path.lstat()
    if not stat.S_ISREG(named.st_mode):
        raise PackagingError("regular artifact required")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(descriptor)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > maximum
                or _signature(named) != _signature(info)):
            raise PackagingError("bounded unaliased artifact required")
        digest, count = hashlib.sha256(), 0
        while data := os.read(descriptor, 65536):
            count += len(data)
            if count > info.st_size:
                raise PackagingError("artifact grew while reading")
            digest.update(data)
        if (count != info.st_size or _signature(info) != _signature(os.fstat(descriptor))
                or _signature(info) != _signature(path.lstat())):
            raise PackagingError("artifact identity changed while reading")
        return ("file", stat.S_IMODE(info.st_mode), count, digest.hexdigest(), _signature(info))
    finally:
        os.close(descriptor)


def _snapshot(root):
    entries, total = {}, 0

    def walk(path):
        nonlocal total
        if len(entries) >= MAX_ENTRIES or len(path.relative_to(root).parts) > 64:
            raise PackagingError("artifact graph bound exceeded")
        name = path.relative_to(root).as_posix()
        if len(os.fsencode(name)) > 1024 or any(ord(c) < 32 for c in name):
            raise PackagingError("unsupported archive member name")
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            entries[name] = ("directory", stat.S_IMODE(info.st_mode), 0, "", _signature(info))
            children = []
            with os.scandir(path) as iterator:
                for child in iterator:
                    if len(entries) + len(children) >= MAX_ENTRIES:
                        raise PackagingError("artifact graph bound exceeded")
                    children.append(Path(child.path))
            for child in sorted(children):
                walk(child)
            if _signature(info) != _signature(path.lstat()):
                raise PackagingError("directory changed during capture")
        elif stat.S_ISREG(info.st_mode):
            entries[name] = _record(path)
            total += entries[name][2]
        else:
            raise PackagingError("linked or special artifact member")
        if total > MAX_TOTAL:
            raise PackagingError("artifact total-byte bound exceeded")

    _real_path(root)
    if not root.is_dir():
        raise PackagingError("artifact root is not a directory")
    walk(root)
    return entries


def _contents(rows):
    return {name: row[:4] for name, row in rows.items()}


def _bytes(path, row, maximum=MAX_TEXT):
    if row[0] != "file" or row[2] > maximum:
        raise PackagingError("source text bound exceeded")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        if _signature(os.fstat(descriptor)) != row[4]:
            raise PackagingError("source identity changed before text read")
        result = bytearray()
        while chunk := os.read(descriptor, min(65536, row[2] + 1 - len(result))):
            result.extend(chunk)
            if len(result) > row[2]:
                raise PackagingError("source text grew")
        if (len(result) != row[2] or hashlib.sha256(result).hexdigest() != row[3]
                or _signature(os.fstat(descriptor)) != row[4]
                or _signature(path.lstat()) != row[4]):
            raise PackagingError("source text changed while reading")
        return bytes(result)
    finally:
        os.close(descriptor)


def _copy(source, destination, rows):
    for name, row in rows.items():
        target = destination if name == "." else destination / name
        if row[0] == "directory":
            target.mkdir(mode=0o700)
            continue
        descriptor = os.open(source / name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            if _signature(os.fstat(descriptor)) != row[4]:
                raise PackagingError("source changed before copy")
            digest, count = hashlib.sha256(), 0
            with target.open("xb") as output:
                while data := os.read(descriptor, 65536):
                    count += len(data)
                    if count > row[2]:
                        raise PackagingError("source grew during copy")
                    digest.update(data)
                    output.write(data)
                output.flush()
                os.fchmod(output.fileno(), row[1])
                os.fsync(output.fileno())
            if (count != row[2] or digest.hexdigest() != row[3]
                    or _signature(os.fstat(descriptor)) != row[4]
                    or _signature((source / name).lstat()) != row[4]):
                raise PackagingError("source changed during copy")
        finally:
            os.close(descriptor)
    for name, row in reversed(list(rows.items())):
        if row[0] == "directory":
            (destination if name == "." else destination / name).chmod(row[1])
    if _contents(_snapshot(destination)) != _contents(rows):
        raise PackagingError("copied Runtime differs")


def _json(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise PackagingError("duplicate JSON key")
            result[key] = value
        return result
    def invalid(_):
        raise PackagingError("non-JSON constant")
    if len(data) > MAX_TEXT:
        raise PackagingError("JSON byte bound exceeded")
    try:
        value = json.loads(data.decode("utf-8"), object_pairs_hook=unique, parse_constant=invalid)
    except (UnicodeError, ValueError, RecursionError):
        raise PackagingError("invalid bounded JSON") from None
    def depth(item, level=0):
        if level > 32:
            raise PackagingError("JSON depth bound exceeded")
        if isinstance(item, dict):
            for child in item.values():
                depth(child, level + 1)
        elif isinstance(item, list):
            for child in item:
                depth(child, level + 1)
    depth(value)
    return value


def _run(argv, *, cwd=None, environment=None, seconds=120):
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, shell=False, close_fds=True,
                               cwd=cwd, env={} if environment is None else environment)
    buffers, selector = [bytearray(), bytearray()], selectors.DefaultSelector()
    deadline = time.monotonic() + seconds
    try:
        for stream, buffer in zip((process.stdout, process.stderr), buffers):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, buffer)
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PackagingError("packaging tool deadline exceeded")
            events = selector.select(min(0.1, remaining))
            if not events and process.poll() is not None:
                break
            for key, _ in events:
                data = os.read(key.fileobj.fileno(), 4096)
                if not data:
                    selector.unregister(key.fileobj)
                else:
                    if len(data) > 65536 - len(key.data):
                        raise PackagingError("packaging tool stream bound exceeded")
                    key.data.extend(data)
        status = process.wait(timeout=max(0.001, deadline - time.monotonic()))
        if status != 0:
            raise PackagingError("packaging tool refused")
        return bytes(buffers[0])
    finally:
        selector.close()
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=2)
        process.stdout.close()
        process.stderr.close()


def _native(architecture):
    if sys.platform != "linux" or platform.machine().lower() != TARGETS[architecture][3]:
        raise PackagingError("matching native GNU/Linux host required")
    release = platform.freedesktop_os_release()
    if release.get("ID") != "ubuntu" or not re.fullmatch(r"[0-9]{2}\.[0-9]{2}", release.get("VERSION_ID", "")):
        raise PackagingError("native Ubuntu dependency-analysis host required")
    return release["VERSION_ID"]


def _verify(root):
    return _json(_run([str(root / ELF_PATHS[0]), "verify"]))


def _elf_headers(root, architecture, rows):
    # Admit captured native bytes before any executable is invoked. A matching
    # host/request alone does not establish the input controller's ISA.
    for name in ELF_PATHS:
        if name not in rows or rows[name][0] != "file":
            raise PackagingError("native executable missing from captured graph")
        descriptor = os.open(root / name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            info = os.fstat(descriptor)
            if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                    or _signature(info) != rows[name][4]):
                raise PackagingError("ELF identity changed before header read")
            header = os.read(descriptor, 20)
            if (_signature(os.fstat(descriptor)) != rows[name][4]
                    or _signature((root / name).lstat()) != rows[name][4]):
                raise PackagingError("ELF identity changed during header read")
        finally:
            os.close(descriptor)
        if (len(header) != 20 or header[:7] != b"\x7fELF\x02\x01\x01"
                or int.from_bytes(header[18:20], "little") != TARGETS[architecture][2]):
            raise PackagingError("native executable architecture differs")


def _report(value, root, request, version, rows):
    if (not isinstance(value, dict) or value.get("schemaVersion") != CLI_SCHEMA
            or value.get("ok") is not True or not isinstance(value.get("result"), dict)):
        raise PackagingError("production verify envelope refused")
    report = value["result"]
    # BundleTarget omits minimumOsVersion when None; Linux's canonical target
    # has exactly these three fields, rather than a serialized null fourth field.
    target = {"os": "linux", "architecture": request.architecture,
              "targetTriple": TARGETS[request.architecture][1]}
    if (set(report) != REPORT_KEYS or report["bundleRoot"] != str(root) or report["target"] != target
            or report["productVersion"] != version or report["sourceState"] != "CLEAN"
            or report["productCommit"] != request.expected_product_commit
            or report["sourceSnapshotSha256"] != request.expected_source_snapshot_sha256
            or report["manifestSha256"] != request.expected_manifest_sha256):
        raise PackagingError("production target/source identity differs")
    if any(not isinstance(report[key], str) or not re.fullmatch(r"[0-9a-f]{64}", report[key]) for key in HASH_KEYS):
        raise PackagingError("production hash malformed")
    if any(type(report[key]) is not int or not 0 < report[key] <= limit for key, limit in
           (("memberCount", 1024), ("totalMemberBytes", 512 * 1024 * 1024))):
        raise PackagingError("production count malformed")
    for key, name in (("launcherSha256", "migration-stream-local"),
                      ("runtimeControllerSha256", ELF_PATHS[0]),
                      ("integrityCliSha256", ELF_PATHS[1]), ("workbenchSha256", ELF_PATHS[2])):
        if name not in rows or rows[name][0] != "file" or rows[name][3] != report[key]:
            raise PackagingError("production report does not bind captured bytes")
    _elf_headers(root, request.architecture, rows)
    return {key: report[key] for key in report if key != "bundleRoot"}


def _dependencies(runtime, work, maintainer):
    control = work / "debian"
    control.mkdir(mode=0o700)
    (control / "control").write_text(
        f"Source: migration-stream\nMaintainer: {maintainer}\n\nPackage: migration-stream\n"
        "Architecture: any\nDescription: Migration Stream engineering layout\n", encoding="ascii")
    output = _run(["/usr/bin/dpkg-shlibdeps", "-O", *["-e" + str(runtime / name) for name in ELF_PATHS]],
                  cwd=work, environment={"PATH": "/usr/bin:/bin", "LC_ALL": "C"}, seconds=60)
    try:
        text = output.decode("ascii").strip()
    except UnicodeError:
        raise PackagingError("native dependencies are not ASCII") from None
    if not re.fullmatch(r"shlibs:Depends=[a-z0-9][a-z0-9+., ()<>=:|~-]{0,8192}", text):
        raise PackagingError("native shared-library dependency analysis missing or malformed")
    return text.removeprefix("shlibs:Depends=")


def _write(path, data, mode=0o444):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as output:
        output.write(data)
        output.flush()
        os.fchmod(output.fileno(), mode)
        os.fsync(output.fileno())


def _tar(path, root, rows, prefix=""):
    class BoundedOutput:
        def __init__(self, output):
            self.output = output

        def write(self, data):
            if self.output.tell() + len(data) > MAX_ARCHIVE:
                raise PackagingError("compressed archive bound exceeded")
            return self.output.write(data)

        def flush(self):
            self.output.flush()

    with path.open("xb") as output, gzip.GzipFile(filename="", mode="wb", fileobj=BoundedOutput(output), mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.PAX_FORMAT) as archive:
            for name, row in rows.items():
                target = root if name == "." else root / name
                member = prefix if name == "." else (prefix + "/" + name if prefix else name)
                if not member:
                    continue
                info = tarfile.TarInfo(member)
                info.mode, info.uid, info.gid, info.mtime = row[1], 0, 0, 0
                info.uname = info.gname = "root"
                if row[0] == "directory":
                    info.type = tarfile.DIRTYPE
                    archive.addfile(info)
                else:
                    if _record(target) != row:
                        raise PackagingError("payload changed before archive read")
                    info.size = row[2]
                    descriptor = os.open(target, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                    with os.fdopen(descriptor, "rb") as source:
                        if _signature(os.fstat(source.fileno())) != row[4]:
                            raise PackagingError("payload identity changed before archive read")
                        archive.addfile(info, source)
                        if _signature(os.fstat(source.fileno())) != row[4]:
                            raise PackagingError("payload changed during archive read")
                    if _record(target) != row:
                        raise PackagingError("payload changed during archive read")
        if output.tell() > MAX_ARCHIVE:
            raise PackagingError("compressed archive bound exceeded")
    _record(path, MAX_ARCHIVE)


def _ar(path, members):
    with path.open("xb") as output:
        output.write(b"!<arch>\n")
        for name, source in members:
            data = source if isinstance(source, bytes) else None
            record = None if data is not None else _record(source, MAX_ARCHIVE)
            size = len(data) if data is not None else record[2]
            header = f"{name + '/':<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{size:<10}`\n".encode("ascii")
            if len(header) != 60:
                raise PackagingError("Debian ar header bound exceeded")
            output.write(header)
            if data is not None:
                output.write(data)
            else:
                descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                with os.fdopen(descriptor, "rb") as artifact:
                    if _signature(os.fstat(artifact.fileno())) != record[4]:
                        raise PackagingError("Debian member identity changed")
                    count = 0
                    while chunk := artifact.read(65536):
                        count += len(chunk)
                        if count > size:
                            raise PackagingError("Debian member grew")
                        output.write(chunk)
                    if count != size or _signature(os.fstat(artifact.fileno())) != record[4]:
                        raise PackagingError("Debian member changed")
                if _record(source, MAX_ARCHIVE) != record:
                    raise PackagingError("Debian member changed")
            if size % 2:
                output.write(b"\n")
            if output.tell() > MAX_ARCHIVE:
                raise PackagingError("Debian archive bound exceeded")
        output.flush()
        os.fsync(output.fileno())
    _record(path, MAX_ARCHIVE)


def _publish(stage, destination):
    # Native no-replace directory publication; no racy exists()+rename fallback.
    if sys.platform != "linux" or stage.parent != destination.parent:
        raise PackagingError("native same-parent publication required")
    identity = stage.lstat()
    library = ctypes.CDLL(None, use_errno=True)
    rename = getattr(library, "renameat2", None)
    if rename is None:
        raise PackagingError("native no-replace publication unavailable")
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    parent = os.open(stage.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        named = os.stat(stage.name, dir_fd=parent, follow_symlinks=False)
        if not stat.S_ISDIR(named.st_mode) or (named.st_dev, named.st_ino) != (identity.st_dev, identity.st_ino):
            raise PackagingError("publication directory changed")
        if rename(parent, os.fsencode(stage.name), parent, os.fsencode(destination.name), 1) != 0:
            raise PackagingError("create-only publication refused")
        os.fsync(parent)
    finally:
        os.close(parent)


def _package(request, *, verify, dependencies, native, publish, repository=None):
    repository = Path(__file__).resolve().parents[1] if repository is None else repository
    if (request.engineering_policy != POLICY or request.architecture not in TARGETS
            or not re.fullmatch(r"[0-9a-f]{40}", request.expected_product_commit)
            or any(not re.fullmatch(r"[0-9a-f]{64}", value) for value in
                   (request.expected_source_snapshot_sha256, request.expected_manifest_sha256))
            or not re.fullmatch(r"[A-Za-z][A-Za-z0-9 .'-]{0,99} <[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9.-]+>", request.maintainer)
            or type(request.revision) is not int or not 1 <= request.revision <= 9999):
        raise PackagingError("explicit source, policy or packaging identity invalid")
    _real_path(request.destination, absent=True)
    _real_path(request.runtime)
    if request.runtime == request.destination or request.runtime in request.destination.parents or request.destination in request.runtime.parents:
        raise PackagingError("Runtime and output overlap")
    cargo_path = repository / "Cargo.toml"
    cargo = tomllib.loads(_bytes(cargo_path, _record(cargo_path)).decode("utf-8"))["package"]
    if cargo["name"] != "migration-stream" or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", cargo["version"]):
        raise PackagingError("actual Cargo package identity unsupported")
    version = cargo["version"]
    ubuntu = native(request.architecture)
    source = _snapshot(request.runtime)
    _elf_headers(request.runtime, request.architecture, source)
    report = _report(verify(request.runtime), request.runtime, request, version, source)
    if _snapshot(request.runtime) != source:
        raise PackagingError("source changed during production verification")
    stage = Path(tempfile.mkdtemp(prefix=".linux-package-result-", dir=request.destination.parent))
    work = Path(tempfile.mkdtemp(prefix=".linux-package-work-", dir=request.destination.parent))
    # Both directories are owned private partials. They are deliberately retained
    # on refusal; cleanup never traverses uncertain Runtime/source/user trees.
    portable = work / "portable"
    portable.mkdir(mode=0o700)
    runtime = portable / "Runtime"
    _copy(request.runtime, runtime, source)
    copied = _snapshot(runtime)
    _elf_headers(runtime, request.architecture, copied)
    if _report(verify(runtime), runtime, request, version, copied) != report:
        raise PackagingError("copied production verification differs")
    if _snapshot(runtime) != copied or _snapshot(request.runtime) != source:
        raise PackagingError("source or staged Runtime changed during verification")
    dependency_work = work / "dependency-analysis"
    dependency_work.mkdir(mode=0o700)
    depends = dependencies(runtime, dependency_work, request.maintainer)
    if not isinstance(depends, str) or not re.fullmatch(r"[a-z0-9][a-z0-9+., ()<>=:|~-]{0,8192}", depends):
        raise PackagingError("native dependency result missing or malformed")
    if _snapshot(runtime) != copied or _snapshot(request.runtime) != source:
        raise PackagingError("Runtime changed during dependency analysis")
    helper_path = repository / "native/linux/packaged_launcher.py"
    spec = importlib.util.spec_from_file_location("packaged_linux_desktop", helper_path)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)  # Reviewed local composition definitions only.
    for name in ("packaged_launcher.py", "document_open_launcher.py"):
        path = repository / "native/linux" / name
        row = _record(path)
        _write(portable / name, _bytes(path, row))
        if _record(path) != row or _record(portable / name)[3] != row[3]:
            raise PackagingError("launcher source changed while copying")
    for name in ("LICENSE", "NOTICE"):
        path = repository / name
        row = _record(path)
        _write(portable / name, _bytes(path, row))
        if _record(path) != row or _record(portable / name)[3] != row[3]:
            raise PackagingError("license source changed while copying")
    debian_version = f"{version}+git{request.expected_product_commit[:12]}-{request.revision}"
    portable_name = f"migration-stream-{debian_version}-linux-{request.architecture}-gnu"
    instructions = ("Migration Stream local engineering package; no platform support or release qualification.\n"
                    "Requires Python >=3.11, xdg-utils and matching native GNU/Linux shared libraries.\n"
                    f"Native Ubuntu {ubuntu} dependency analysis: {depends}\n"
                    "Open: /usr/bin/python3 -I /absolute/layout/packaged_launcher.py /absolute/object.mstream\n"
                    "Optional user-only registration (existing user-owned DATA_HOME, ordinary ASCII layout path):\n"
                    "  /usr/bin/python3 -I /absolute/layout/packaged_launcher.py --register-desktop /absolute/DATA_HOME\n"
                    "Registration creates only MIME/Open-with entries and updates user caches; it never sets a default.\n"
                    "Controller environment is empty: state uses verified root-owned sticky /tmp, in\n"
                    "migration-stream-runtime-<uid>/session-<key>; no state is installed inside Runtime.\n"
                    "No root installer, startup service or automatic registration is included.\n")
    _write(portable / "README.txt", instructions.encode("utf-8"))
    payload = work / "deb-payload"
    payload.mkdir(mode=0o700)
    library_root = payload / "usr/lib/migration-stream"
    library_root.parent.mkdir(parents=True, mode=0o755)
    _copy(portable, library_root, _snapshot(portable))
    _write(payload / "usr/share/applications/migration-stream.desktop",
           helper.desktop_entry(Path("/usr/lib/migration-stream/packaged_launcher.py")).encode("ascii"))
    _write(payload / "usr/share/mime/packages/migration-stream.xml", helper.MIME_XML.encode("utf-8"))
    _write(payload / "usr/share/doc/migration-stream/copyright", (portable / "LICENSE").read_bytes())
    _write(payload / "usr/share/doc/migration-stream/NOTICE", (portable / "NOTICE").read_bytes())
    for directory in [payload, *[path for path in payload.rglob("*") if path.is_dir() and "Runtime" not in path.parts]]:
        directory.chmod(0o755)
    portable.chmod(0o755)
    portable_rows, payload_rows = _snapshot(portable), _snapshot(payload)
    control = work / "control"
    control.mkdir(mode=0o755)
    size = (sum(row[2] for row in payload_rows.values()) + 1023) // 1024
    control_text = (f"Package: {cargo['name']}\nVersion: {debian_version}\nArchitecture: {TARGETS[request.architecture][0]}\n"
                    f"Maintainer: {request.maintainer}\nInstalled-Size: {size}\n"
                    f"Depends: python3 (>= 3.11), xdg-utils, shared-mime-info, desktop-file-utils, {depends}\n"
                    "Section: utils\nPriority: optional\nDescription: Migration Stream local engineering desktop layout\n"
                    " Opens a bound .mstream object through the immutable assisted runtime.\n"
                    " No supported-platform or official binary-release claim.\n")
    _write(control / "control", control_text.encode("ascii"))
    _tar(work / "control.tar.gz", control, _snapshot(control))
    _tar(work / "data.tar.gz", payload, payload_rows)
    deb_name = f"migration-stream_{debian_version}_{TARGETS[request.architecture][0]}.deb"
    tar_name = portable_name + ".tar.gz"
    _ar(stage / deb_name, (("debian-binary", b"2.0\n"), ("control.tar.gz", work / "control.tar.gz"),
                          ("data.tar.gz", work / "data.tar.gz")))
    _tar(stage / tar_name, portable, portable_rows, portable_name)
    if (_snapshot(request.runtime) != source or _snapshot(runtime) != copied
            or _snapshot(portable) != portable_rows or _snapshot(payload) != payload_rows):
        raise PackagingError("final source/payload graph changed")
    artifacts = {}
    for name in (deb_name, tar_name):
        row = _record(stage / name, MAX_ARCHIVE)
        artifacts[name] = {"bytes": row[2], "sha256": row[3]}
    receipt = {"schemaVersion": "migration-stream/linux-desktop-package/v1", "engineeringPolicy": POLICY,
               "package": cargo["name"], "version": debian_version, "architecture": TARGETS[request.architecture][0],
               "nativeUbuntuSeries": ubuntu, "nativeSharedLibraryDepends": depends, "runtime": report,
               "runtimeFileGraph": {name: list(row[:4]) for name, row in source.items()},
               "artifacts": artifacts, "productionVerifyCalls": 2, "workingDirectoryRetained": True,
               "controllerEnvironment": "empty", "runtimeStateBase": "/tmp", "supportedPlatformClaim": False}
    data = (json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    if len(data) > MAX_TEXT:
        raise PackagingError("package receipt bound exceeded")
    _write(stage / "package-receipt.json", data)
    publish(stage, request.destination)
    return receipt


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--architecture", choices=tuple(TARGETS), required=True)
    for name in ("expected-product-commit", "expected-source-snapshot-sha256", "expected-manifest-sha256", "maintainer"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--revision", type=int, required=True)
    parser.add_argument("--engineering-policy", required=True, choices=(POLICY,))
    args = parser.parse_args(arguments)
    def expired(_signum, _frame):
        raise PackagingError("whole packaging deadline exceeded")
    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, 360)
    try:
        receipt = _package(Request(**vars(args)), verify=_verify, dependencies=_dependencies,
                           native=_native, publish=_publish)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print("Linux packaging refused; retained private partials may exist.", file=sys.stderr)
        return 1
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)
    print(json.dumps({"ok": True, "artifacts": receipt["artifacts"]}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
