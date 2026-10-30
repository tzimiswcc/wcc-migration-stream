#!/usr/bin/env python3
"""One manual, native Ubuntu build/package/read-only payload qualification.

No OS installation, registration, service, application/browser or binary upload.
The unchanged Runtime builder makes four metadata/file probes; the reviewed
packager makes two Verify probes. Inspection adds no product process.
Command failure evidence exposes only a fixed stage/category and numeric status;
raw stderr, arguments and environment remain private. No automatic retry.
"""
from __future__ import annotations

import ctypes
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import tomllib


ROOT = Path(__file__).resolve().parents[1]
LANES = {
    "x64": ("x86_64", "x86_64-unknown-linux-gnu", "amd64", 62),
    "arm64": ("aarch64", "aarch64-unknown-linux-gnu", "arm64", 183),
}
MAX_LOG = 64 * 1024 * 1024
MAX_ARCHIVE = 1024 * 1024 * 1024
MAX_EXPANDED = 530 * 1024 * 1024
MAINTAINER = r"[A-Za-z][A-Za-z0-9 .'-]{0,99} <[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9.-]+>"


class Refused(ValueError):
    pass


class CommandFailure(Refused):
    """Allowlisted evidence only; never retain an OS message or command payload."""

    def __init__(self, stage, category, code=None):
        require(re.fullmatch(r"[a-z-]{1,40}", stage))
        require(category in {"deadline", "output-cap", "exit", "os-error", "wait-timeout"})
        require(code is None or (type(code) is int and -(2**31) <= code < 2**31))
        self.evidence = {"stage": stage, "category": category, "code": code}
        super().__init__("Linux command refused")


def require(value):
    if not value:
        raise Refused("Linux qualification refused")


def helpers(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def admission(environment, system, machine, series):
    architecture = environment.get("QUALIFICATION_ARCHITECTURE")
    require(architecture in LANES)
    expected = environment.get("EXPECTED_SOURCE_COMMIT", "")
    require(re.fullmatch(r"[0-9a-f]{40}", expected))
    require(environment.get("GITHUB_SHA") == expected)
    require(environment.get("GITHUB_EVENT_NAME") == "workflow_dispatch")
    require(environment.get("GITHUB_RUN_ATTEMPT") == "1")
    require(re.fullmatch(r"[0-9]{1,20}", environment.get("GITHUB_RUN_ID", "")))
    require(environment.get("GITHUB_REPOSITORY") == "tzimiswcc/wcc-migration-stream")
    require(environment.get("RUNNER_OS") == "Linux" and
            environment.get("RUNNER_ARCH") == ("ARM64" if architecture == "arm64" else "X64"))
    require(system == "linux" and machine == LANES[architecture][0])
    require(series == {"ID": "ubuntu", "VERSION_ID": "24.04"})
    require(environment.get("RUSTUP_TOOLCHAIN") == "1.99.1-" + LANES[architecture][1])
    require(re.fullmatch(MAINTAINER, environment.get("DEBIAN_MAINTAINER", "")))
    for key in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET",
                "RUSTC", "RUSTDOC", "MACOSX_DEPLOYMENT_TARGET"):
        require(not environment.get(key))
    require(not any(key.startswith("VITE_") for key in environment))
    return architecture, expected


def executable(path, machine=None):
    path = Path(path).resolve(strict=True)
    before = path.stat()
    require(stat.S_ISREG(before.st_mode) and before.st_mode & 0o111 and before.st_size <= 256 * 1024 * 1024)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        opened = os.fstat(descriptor)
        require((opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns) ==
                (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns))
        digest, count, header = hashlib.sha256(), 0, b""
        while data := os.read(descriptor, 65536):
            if not header:
                header = data[:20]
            count += len(data)
            require(count <= before.st_size)
            digest.update(data)
        after = os.fstat(descriptor)
        signature = lambda item: (item.st_dev, item.st_ino, item.st_mode, item.st_size,
                                  item.st_mtime_ns, item.st_ctime_ns)
        require(count == before.st_size and signature(opened) == signature(after) == signature(path.lstat()))
        if machine is not None:
            require(header[:7] == b"\x7fELF\x02\x01\x01" and len(header) == 20 and
                    int.from_bytes(header[18:20], "little") == machine)
        return path, digest.hexdigest()
    finally:
        os.close(descriptor)


def command(argv, work, label, environment, *, cwd=ROOT, seconds=120,
            stdout_cap=MAX_LOG, stderr_cap=MAX_LOG):
    """Capture only this command; raw diagnostics stay in private job storage."""
    require(re.fullmatch(r"[a-z-]{1,40}", label))
    output = work / (label + ".stdout.private")
    error = work / (label + ".stderr.private")
    streams = [output.open("xb"), error.open("xb")]
    for stream in streams:
        os.fchmod(stream.fileno(), 0o600)
    process, selector = None, selectors.DefaultSelector()
    def within_budget():
        if time.monotonic() >= deadline:
            raise CommandFailure(label, "deadline")
    try:
        process = subprocess.Popen([str(item) for item in argv], stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   cwd=cwd, env=environment, close_fds=True,
                                   start_new_session=True)
        for index, pipe in enumerate((process.stdout, process.stderr)):
            os.set_blocking(pipe.fileno(), False)
            selector.register(pipe, selectors.EVENT_READ, index)
        counts, caps, deadline = [0, 0], [stdout_cap, stderr_cap], time.monotonic() + seconds
        while selector.get_map():
            within_budget()
            exited = process.poll() is not None
            remaining = deadline - time.monotonic()
            within_budget()
            events = selector.select(0 if exited else min(0.05, remaining))
            within_budget()
            for key, _ in events:
                data = os.read(key.fileobj.fileno(), 65536)
                within_budget()
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                index = key.data
                counts[index] += len(data)
                if counts[index] > caps[index]:
                    raise CommandFailure(label, "output-cap")
                streams[index].write(data)
            if exited and not events:
                break
        remaining = deadline - time.monotonic()
        within_budget()
        status = process.wait(timeout=remaining)
        within_budget()
        if status != 0:
            raise CommandFailure(label, "exit", status)
        for stream in streams:
            stream.flush()
            os.fsync(stream.fileno())
        within_budget()
        return output
    except subprocess.TimeoutExpired:
        raise CommandFailure(label, "wait-timeout") from None
    except OSError as error:
        code = error.errno if type(error.errno) is int and -(2**31) <= error.errno < 2**31 else None
        raise CommandFailure(label, "os-error", code) from None
    finally:
        selector.close()
        if process is not None:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=2)
            process.stdout.close()
            process.stderr.close()
        for stream in streams:
            stream.close()


def assembly_result(data, runtime, parse):
    """The unchanged builder's final stdout is its pretty-printed JSON result."""
    require(len(data) <= 256 * 1024)
    start = data.rfind(b"\n{")
    payload = data[start + 1:] if start >= 0 else data
    value = parse(payload)
    require(isinstance(value, dict) and set(value) == {"schemaVersion", "ok", "result"})
    require(value["schemaVersion"] == "migration-stream/assisted-local-runtime/assembly/v1" and
            value["ok"] is True and isinstance(value["result"], dict))
    result = value["result"]
    require(set(result) == {"bundleDirectory", "manifestSha256", "memberCount", "totalMemberBytes"})
    require(result["bundleDirectory"] == str(runtime) and
            isinstance(result["manifestSha256"], str) and
            re.fullmatch(r"[0-9a-f]{64}", result["manifestSha256"]))
    for name, maximum in (("memberCount", 1024), ("totalMemberBytes", 512 * 1024 * 1024)):
        require(type(result[name]) is int and 0 < result[name] <= maximum)
    return result


def manifest_pins(value, assembly, architecture, commit, version):
    require(isinstance(value, dict) and isinstance(value.get("product"), dict))
    product = value["product"]
    require(value.get("target") == {"os": "linux", "architecture": architecture,
                                   "targetTriple": LANES[architecture][1]})
    require(product.get("name") == "migration-stream" and product.get("version") == version and
            product.get("commit") == commit and product.get("sourceState") == "CLEAN")
    require(value.get("manifestSha256") == assembly["manifestSha256"])
    snapshot = product.get("sourceSnapshotSha256")
    require(isinstance(snapshot, str) and re.fullmatch(r"[0-9a-f]{64}", snapshot))
    return snapshot


def deb_layout(path):
    with path.open("rb") as stream:
        length = path.stat().st_size
        require(length <= MAX_ARCHIVE)
        require(stream.read(8) == b"!<arch>\n")
        for expected in ("debian-binary", "control.tar.gz", "data.tar.gz"):
            header = stream.read(60)
            require(len(header) == 60 and header[58:] == b"`\n")
            require(header[:16].decode("ascii").strip().removesuffix("/") == expected)
            size_text = header[48:58].decode("ascii").strip()
            require(re.fullmatch(r"[0-9]{1,10}", size_text))
            size = int(size_text)
            require(0 < size <= MAX_ARCHIVE and stream.tell() + size + size % 2 <= length)
            if expected == "debian-binary":
                require(size == 4 and stream.read(size) == b"2.0\n")
            else:
                stream.seek(size, 1)
            if size % 2:
                require(stream.read(1) == b"\n")
        require(stream.read(1) == b"")


def control_fields(data):
    require(len(data) <= 64 * 1024)
    fields = {}
    for line in data.decode("ascii").splitlines():
        key, separator, value = line.partition(": ")
        require(separator and key not in fields)
        fields[key] = value
    require(set(fields) == {"Package", "Version", "Architecture", "Maintainer", "Depends"})
    return fields


def artifact_state(package, root, names):
    package._real_path(root)
    info = root.lstat()
    require(stat.S_ISDIR(info.st_mode) and set(os.listdir(root)) == set(names))
    rows = {".": ("directory", stat.S_IMODE(info.st_mode), 0, "", package._signature(info))}
    for name in names:
        rows[name] = package._record(root / name, MAX_ARCHIVE if name != "package-receipt.json" else 1024 * 1024)
    require(package._signature(root.lstat()) == package._signature(info))
    return rows


def extract_ustar(path, destination, *, compressed=False):
    """Read only regular/directory USTAR into a new private inspection tree."""
    destination.mkdir(mode=0o700)
    raw = path.open("rb")
    stream = gzip.GzipFile(fileobj=raw, mode="rb") if compressed else raw
    seen, directories, expanded, total = set(), [], 0, 0
    def read(size):
        nonlocal expanded
        value = stream.read(size)
        expanded += len(value)
        require(len(value) == size and expanded <= MAX_EXPANDED)
        return value
    try:
        while True:
            header = read(512)
            if header == bytes(512):
                tail = stream.read(16385)
                require(512 <= len(tail) <= 16384 and expanded + len(tail) <= MAX_EXPANDED and
                        not any(tail) and stream.read(1) == b"")
                break
            require(header[257:265] == b"ustar\0" + b"00" and header[156:157] in (b"0", b"5"))
            require(int(header[148:156].strip(b" \0"), 8) == sum(header[:148] + b" " * 8 + header[156:]))
            name = header[:100].split(b"\0", 1)[0].decode("utf-8")
            prefix = header[345:500].split(b"\0", 1)[0].decode("utf-8")
            name = (prefix + "/" if prefix else "") + name
            if header[156:157] == b"5" and name.endswith("/"):
                name = name[:-1]
            parts = name.split("/")
            require(name and not PurePosixPath(name).is_absolute() and len(parts) <= 64 and
                    all(part not in ("", ".", "..") for part in parts) and
                    not any(ord(char) < 32 or ord(char) == 127 for char in name) and name not in seen)
            seen.add(name)
            require(len(seen) <= 4096)
            size = int(header[124:136].strip(b" \0"), 8)
            mode = int(header[100:108].strip(b" \0"), 8)
            require(0 <= size <= 128 * 1024 * 1024 and mode in (0o444, 0o555, 0o755))
            require(int(header[108:116].strip(b" \0"), 8) == 0 and
                    int(header[116:124].strip(b" \0"), 8) == 0)
            target = destination.joinpath(*parts)
            require(target.parent.is_dir() and not target.parent.is_symlink())
            if header[156:157] == b"5":
                require(size == 0)
                target.mkdir(mode=0o700)
                directories.append((target, mode))
            else:
                total += size
                require(total <= 513 * 1024 * 1024)
                with target.open("xb") as output:
                    remaining = size
                    while remaining:
                        data = read(min(65536, remaining))
                        output.write(data)
                        remaining -= len(data)
                    os.fchmod(output.fileno(), mode)
                    output.flush()
                    os.fsync(output.fileno())
                padding = (-size) % 512
                if padding:
                    require(not any(read(padding)))
        for target, mode in reversed(directories):
            target.chmod(mode)
        destination.chmod(0o755)
    finally:
        stream.close()
        raw.close()


def inspect_payload(package, source, portable, installed, adapter, commit, version, architecture):
    prefix = "migration-stream-" + version + "+git" + commit[:12] + "-1-linux-" + architecture + "-gnu"
    top = package._snapshot(portable)
    require(set(name.split("/", 1)[0] for name in top if name != ".") == {prefix})
    layout = portable / prefix
    rows = package._snapshot(layout)
    require(package._contents(package._snapshot(layout / "Runtime")) == package._contents(source))
    expected = {".", "packaged_launcher.py", "document_open_launcher.py", "LICENSE", "NOTICE", "README.txt"}
    expected |= {"Runtime" if name == "." else "Runtime/" + name for name in source}
    require(set(rows) == expected)
    for name, relative in (("packaged_launcher.py", "native/linux/packaged_launcher.py"),
                           ("document_open_launcher.py", "native/linux/document_open_launcher.py"),
                           ("LICENSE", "LICENSE"), ("NOTICE", "NOTICE")):
        require(rows[name][0:2] == ("file", 0o444) and rows[name][3] == package._record(ROOT / relative)[3])
    require(package._contents(package._snapshot(installed / "usr/lib/migration-stream")) == package._contents(rows))
    all_rows = package._snapshot(installed)
    library = "usr/lib/migration-stream"
    names = {".", "usr", "usr/lib", "usr/share", "usr/share/applications", "usr/share/mime",
             "usr/share/mime/packages", "usr/share/doc", "usr/share/doc/migration-stream"}
    names |= {library if name == "." else library + "/" + name for name in rows}
    files = {
        "usr/share/applications/migration-stream.desktop": adapter.desktop_entry(Path("/usr/lib/migration-stream/packaged_launcher.py")).encode("ascii"),
        "usr/share/mime/packages/migration-stream.xml": adapter.MIME_XML.encode("utf-8"),
        "usr/share/doc/migration-stream/copyright": (ROOT / "LICENSE").read_bytes(),
        "usr/share/doc/migration-stream/NOTICE": (ROOT / "NOTICE").read_bytes(),
    }
    require(set(all_rows) == names | set(files))
    for name, data in files.items():
        require(all_rows[name][:4] == ("file", 0o444, len(data), hashlib.sha256(data).hexdigest()))
    return len([row for row in all_rows.values() if row[0] == "file"])


def qualify(environment):
    series = platform.freedesktop_os_release() if sys.platform == "linux" else {}
    architecture, commit = admission(environment, sys.platform, platform.machine(),
                                     {key: series.get(key) for key in ("ID", "VERSION_ID")})
    require(sys.flags.isolated and sys.version_info >= (3, 11) and ctypes.sizeof(ctypes.c_void_p) == 8)
    package = helpers("linux_qualification_packager", "scripts/package-linux-desktop.py")
    adapter = helpers("linux_qualification_adapter", "native/linux/packaged_launcher.py")
    machine, triple, deb_arch, elf = LANES[architecture]
    python, python_sha = executable(sys.executable, elf)
    temp = Path(environment["RUNNER_TEMP"])
    package._real_path(temp)
    require(temp.is_dir() and temp.stat().st_uid == os.geteuid() and shutil.disk_usage(temp).free >= 8 * 1024**3)
    require(os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") >= 8 * 1024**3)
    work = Path(tempfile.mkdtemp(prefix="migration-stream-linux-qualification-", dir=temp))
    for child in ("npm-cache", "cargo-home", "tmp"):
        (work / child).mkdir(mode=0o700)
    for child in ("user.npmrc", "global.npmrc"):
        (work / child).touch(mode=0o600, exist_ok=False)
    node, node_sha = executable(shutil.which("node"), elf)
    bootstrap_npm, bootstrap_npm_sha = executable(shutil.which("npm"))
    require(bootstrap_npm == (node.parent.parent / "lib/node_modules/npm/bin/npm-cli.js").resolve(strict=True))
    rustup, rustup_sha = executable(shutil.which("rustup"), elf)
    git, git_sha = executable("/usr/bin/git", elf)
    dpkg, dpkg_sha = executable("/usr/bin/dpkg-deb", elf)
    _, shlibs_sha = executable("/usr/bin/dpkg-shlibdeps")
    env = {"PATH": str(node.parent) + ":" + str(rustup.parent) + ":/usr/bin:/bin",
           "HOME": environment["HOME"], "LC_ALL": "C", "RUSTUP_TOOLCHAIN": "1.99.1-" + triple,
           "CARGO_HOME": str(work / "cargo-home"), "CARGO_NET_RETRY": "0", "CARGO_BUILD_JOBS": "2",
           "TMPDIR": str(work / "tmp"), "NPM_CONFIG_USERCONFIG": str(work / "user.npmrc"),
           "NPM_CONFIG_GLOBALCONFIG": str(work / "global.npmrc"), "npm_config_cache": str(work / "npm-cache"),
           "npm_config_fetch_retries": "0", "npm_config_progress": "false", "GIT_CONFIG_NOSYSTEM": "1"}
    def run(argv, label, **options):
        print("LINUX_QUALIFICATION stage=" + label, flush=True)
        return command(argv, work, label, env, **options)
    def small(argv, label):
        return run(argv, label, stdout_cap=65536, stderr_cap=65536).read_bytes().decode("utf-8").strip()
    require(small([git, "rev-parse", "HEAD"], "source-head") == commit)
    tree = small([git, "rev-parse", "HEAD^{tree}"], "source-tree")
    require(re.fullmatch(r"[0-9a-f]{40}", tree))
    require(not small([git, "status", "--porcelain=v1", "--untracked-files=all"], "source-clean"))
    dpkg_version = small([dpkg, "--version"], "dpkg-admission").splitlines()[0]
    require(re.fullmatch(r"Debian .{1,64}version [0-9][0-9A-Za-z.+:~_-]{0,63}(?: .{0,64})?", dpkg_version))
    require(small([node, "-p", "'v'+process.versions.node+' '+process.arch"], "node-admission") == "v24.22.1 " + architecture)
    require((ROOT / "web/.node-version").read_text().strip() == "24.22.1")
    require(json.loads((ROOT / "web/package.json").read_text())["packageManager"] == "npm@12.2.1")
    run([rustup, "toolchain", "install", "1.99.1-" + triple, "--profile", "minimal"], "rust-preparation", seconds=300)
    cargo, cargo_sha = executable(small([rustup, "which", "--toolchain", "1.99.1-" + triple, "cargo"], "cargo-path"), elf)
    rustc, rustc_sha = executable(small([rustup, "which", "--toolchain", "1.99.1-" + triple, "rustc"], "rustc-path"), elf)
    rust_version = small([rustc, "-vV"], "rust-admission").splitlines()
    require(rust_version[0].startswith("rustc 1.99.1 ") and [x for x in rust_version if x.startswith("host:")] == ["host: " + triple])
    npm_prefix = work / "npm-tool"
    run([node, bootstrap_npm, "install", "--prefix", npm_prefix, "--ignore-scripts", "--no-audit", "--no-fund", "npm@12.2.1"], "npm-preparation", seconds=300)
    npm = npm_prefix / "node_modules/npm/bin/npm-cli.js"
    npm, npm_sha = executable(npm)
    require(small([node, npm, "--version"], "npm-admission") == "12.2.1")
    env["PATH"] = str(cargo.parent) + ":" + str(npm_prefix / "node_modules/.bin") + ":" + str(node.parent) + ":/usr/bin:/bin"
    run([node, npm, "--prefix", "web", "ci", "--ignore-scripts", "--no-audit", "--no-fund"], "npm-cache", seconds=300)
    run([cargo, "fetch", "--locked", "--target", triple], "cargo-cache", seconds=300)
    env.update({"CARGO_NET_OFFLINE": "true", "npm_config_offline": "true"})
    runtime = work / ("migration-stream-local-linux-" + architecture + "-gnu")
    stdout = run(["/usr/bin/bash", ROOT / "scripts/build-assisted-runtime-bundle.sh", "--output", runtime, "--target", triple, "--no-archive"], "runtime-build", seconds=1800)
    with stdout.open("rb") as stream:
        stream.seek(max(0, stdout.stat().st_size - 256 * 1024))
        assembly = assembly_result(stream.read(), runtime, package._json)
    cargo_document = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    version = cargo_document["version"]
    require(cargo_document["name"] == "migration-stream" and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version))
    manifest = package._json(package._bytes(runtime / "bundle-manifest.json", package._record(runtime / "bundle-manifest.json")))
    snapshot = manifest_pins(manifest, assembly, architecture, commit, version)
    source = package._snapshot(runtime)
    output = work / "packages"
    run([python, "-I", "-B", ROOT / "scripts/package-linux-desktop.py", "--runtime", runtime,
         "--destination", output, "--architecture", architecture, "--expected-product-commit", commit,
         "--expected-source-snapshot-sha256", snapshot, "--expected-manifest-sha256", assembly["manifestSha256"],
         "--maintainer", environment["DEBIAN_MAINTAINER"], "--revision", "1", "--engineering-policy", package.POLICY],
        "package-assembly", seconds=360)
    receipt = package._json(package._bytes(output / "package-receipt.json", package._record(output / "package-receipt.json")))
    deb_version = version + "+git" + commit[:12] + "-1"
    deb_name = "migration-stream_" + deb_version + "_" + deb_arch + ".deb"
    tar_name = "migration-stream-" + deb_version + "-linux-" + architecture + "-gnu.tar.gz"
    require(receipt["schemaVersion"] == "migration-stream/linux-desktop-package/v1" and
            receipt["package"] == "migration-stream" and receipt["engineeringPolicy"] == package.POLICY and
            receipt["nativeUbuntuSeries"] == "24.04" and
            receipt["version"] == deb_version and receipt["architecture"] == deb_arch and
            receipt["productionVerifyCalls"] == 2 and receipt["supportedPlatformClaim"] is False)
    require(receipt["runtime"]["target"] == {"os": "linux", "architecture": architecture, "targetTriple": triple} and
            receipt["runtime"]["productVersion"] == version and receipt["runtime"]["sourceState"] == "CLEAN" and
            receipt["runtime"]["productCommit"] == commit and receipt["runtime"]["sourceSnapshotSha256"] == snapshot and
            receipt["runtime"]["manifestSha256"] == assembly["manifestSha256"] and
            receipt["runtime"]["memberCount"] == assembly["memberCount"] and
            receipt["runtime"]["totalMemberBytes"] == assembly["totalMemberBytes"])
    require(receipt["runtimeFileGraph"] == {name: list(row[:4]) for name, row in source.items()})
    artifact_rows = artifact_state(package, output, ("package-receipt.json", deb_name, tar_name))
    require(set(receipt["artifacts"]) == {deb_name, tar_name})
    for name in (deb_name, tar_name):
        row = package._record(output / name, MAX_ARCHIVE)
        require(receipt["artifacts"][name] == {"bytes": row[2], "sha256": row[3]})
    deb_layout(output / deb_name)
    fields = control_fields(run([dpkg, "--field", output / deb_name, "Package", "Version", "Architecture", "Maintainer", "Depends"], "debian-fields", stdout_cap=65536, stderr_cap=65536).read_bytes())
    require(fields == {"Package": "migration-stream", "Version": deb_version, "Architecture": deb_arch,
                       "Maintainer": environment["DEBIAN_MAINTAINER"],
                       "Depends": "python3 (>= 3.11), xdg-utils, shared-mime-info, desktop-file-utils, " + receipt["nativeSharedLibraryDepends"]})
    control = run([dpkg, "--ctrl-tarfile", output / deb_name], "debian-control", stdout_cap=2 * 1024 * 1024, stderr_cap=65536)
    data = run([dpkg, "--fsys-tarfile", output / deb_name], "debian-payload", stdout_cap=MAX_EXPANDED, stderr_cap=65536)
    extract_ustar(control, work / "control-inspection")
    require(set(package._snapshot(work / "control-inspection")) == {".", "control"})
    extract_ustar(data, work / "installed-payload-inspection")
    extract_ustar(output / tar_name, work / "portable-inspection", compressed=True)
    payload_files = inspect_payload(package, source, work / "portable-inspection", work / "installed-payload-inspection", adapter, commit, version, architecture)
    require(artifact_state(package, output, ("package-receipt.json", deb_name, tar_name)) == artifact_rows and package._snapshot(runtime) == source)
    require(small([git, "rev-parse", "HEAD"], "final-head") == commit and
            not small([git, "status", "--porcelain=v1", "--untracked-files=all"], "final-clean"))
    return {"schemaVersion": "migration-stream/linux-package-qualification/v1", "status": "QUALIFIED_OFFLINE_PACKAGE_PAYLOAD",
            "runId": environment["GITHUB_RUN_ID"], "runAttempt": 1, "sourceCommit": commit, "sourceTree": tree,
            "architecture": architecture, "target": triple, "ubuntuSeries": "24.04", "pythonVersion": platform.python_version(),
            "nodeVersion": "24.22.1", "npmVersion": "12.2.1", "rustVersion": "1.99.1", "dpkgDebVersion": dpkg_version,
            "maintainerSha256": hashlib.sha256(environment["DEBIAN_MAINTAINER"].encode("ascii")).hexdigest(),
            "toolSha256": {"python": python_sha, "node": node_sha, "rustup": rustup_sha, "cargo": cargo_sha,
                           "rustc": rustc_sha, "git": git_sha, "dpkgDeb": dpkg_sha, "dpkgShlibdeps": shlibs_sha,
                           "bootstrapNpm": bootstrap_npm_sha, "npmCli": npm_sha},
            "runtimeManifestSha256": assembly["manifestSha256"], "sourceSnapshotSha256": snapshot,
            "runtimeMemberCount": assembly["memberCount"], "runtimeMemberBytes": assembly["totalMemberBytes"],
            "artifacts": receipt["artifacts"], "inspectedPayloadFiles": payload_files, "productMetadataVerifyCalls": 6,
            "osInstallation": False, "appBrowserRegistration": False, "binaryUpload": False}


def main():
    def expired(_signum, _frame):
        raise Refused("qualification whole-operation deadline")
    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, 2600)
    try:
        evidence = qualify(dict(os.environ))
        text = json.dumps(evidence, sort_keys=True, separators=(",", ":"))
        require(len(text.encode("utf-8")) <= 16384)
        print(text)
        return 0
    except CommandFailure as error:
        print("LINUX_QUALIFICATION_FAILURE " + json.dumps(error.evidence, sort_keys=True, separators=(",", ":")), file=sys.stderr)
        return 1
    except Exception:
        print("Linux package qualification refused; private job evidence retained.", file=sys.stderr)
        return 1
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


if __name__ == "__main__":
    raise SystemExit(main())
