#!/usr/bin/env python3
"""Create-only Apple-silicon engineering .app + DMG from trusted built inputs.

No signing, notarization, installation, registration, explicit image attachment
or application launch. The real image tool is used only by an admitted caller.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import tempfile
import time

SPEC = importlib.util.spec_from_file_location(
    "macos_engineering_assembler", Path(__file__).with_name("assemble-macos-engineering-app.py"))
assert SPEC and SPEC.loader
assembler = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = assembler
SPEC.loader.exec_module(assembler)

APP_NAME = "Migration Stream.app"
IMAGE_NAME = "Migration Stream.dmg"
RECEIPT_NAME = "engineering-package.json"
MAX_IMAGE = 1024 * 1024 * 1024
MAX_RECEIPT = 1024 * 1024
SCHEMA = "migration-stream/macos-engineering-package/v1"


class PackagingError(ValueError):
    pass


def _image_record(path: Path):
    assembler._real_path(path)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or not 0 < before.st_size <= MAX_IMAGE:
            raise PackagingError("image is not one bounded unaliased regular file")
        digest, count = hashlib.sha256(), 0
        while data := os.read(descriptor, 65536):
            count += len(data)
            if count > before.st_size:
                raise PackagingError("image changed during capture")
            digest.update(data)
        if count != before.st_size or assembler._signature(before) != assembler._signature(os.fstat(descriptor)) or assembler._signature(before) != assembler._signature(path.lstat()):
            raise PackagingError("image identity changed during capture")
        return {"bytes": count, "sha256": digest.hexdigest()}
    finally:
        os.close(descriptor)


def _run_image_tool(arguments):
    # Fixed OS tool, no shell/PATH/environment lookup, no input/prompt handling.
    deadline = time.monotonic() + 300
    process = subprocess.Popen(["/usr/bin/hdiutil", *arguments], env={}, shell=False,
                               stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    selector = selectors.DefaultSelector()
    counts = {}
    try:
        for stream in (process.stdout, process.stderr):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ)
            counts[stream.fileno()] = 0
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PackagingError("image tool deadline exceeded")
            events = selector.select(min(0.1, remaining))
            if not events and process.poll() is not None:
                break  # Never wait for inherited pipe EOF from descendants.
            for key, _ in events:
                data = os.read(key.fd, 4096)
                if time.monotonic() >= deadline:
                    raise PackagingError("image tool deadline exceeded")
                if not data:
                    selector.unregister(key.fileobj)
                else:
                    counts[key.fd] += len(data)
                    if counts[key.fd] > 65536:
                        raise PackagingError("image tool output exceeds bound")
        status = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        if time.monotonic() >= deadline or status != 0:
            raise PackagingError("image tool refused or failed")
    except subprocess.TimeoutExpired:
        raise PackagingError("image tool deadline exceeded") from None
    finally:
        selector.close()
        try:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        raise PackagingError("owned image child cleanup unresolved") from None
        finally:
            process.stdout.close()
            process.stderr.close()


def _image_commands(source: Path, image: Path):
    return (
        ["create", "-quiet", "-fs", "HFS+", "-format", "UDZO", "-volname", "Migration Stream", "-srcfolder", str(source), str(image)],
        ["verify", "-quiet", str(image)],
    )


def _create_image(source: Path, image: Path, *, runner=_run_image_tool):
    assembler._real_path(image, absent=True)
    create, verify = _image_commands(source, image)
    runner(create)
    captured = _image_record(image)
    runner(verify)
    if _image_record(image) != captured:
        raise PackagingError("image bytes changed during checksum verification")
    return {"format": "UDZO", "filesystem": "HFS+", "checksumVerification": "hdiutil-exit-zero"}


def _package_names(root: Path):
    names = set()
    with os.scandir(root) as entries:
        for entry in entries:
            if len(names) >= 3:
                raise PackagingError("unexpected package root entry")
            names.add(entry.name)
    if names != {APP_NAME, IMAGE_NAME, RECEIPT_NAME}:
        raise PackagingError("package root differs from exact output contract")


def _runtime_identity(validator, root: Path):
    report = assembler._report(validator(root), root)
    if report["sourceState"] != "CLEAN":
        raise PackagingError("desktop candidate requires a clean source runtime")
    return report


def package(request: assembler.Request, *, validator=assembler._verify_runtime,
            image_maker=_create_image, publisher=assembler._publish_noreplace):
    """Internal injection permits fake-artifact tests, never a public bypass."""
    if request.engineering_policy != assembler.POLICY:
        raise PackagingError("explicit unsigned engineering policy required")
    for path in (request.wrapper, request.runtime):
        assembler._real_path(path)
    assembler._real_path(request.destination, absent=True)
    for first, second in ((request.wrapper, request.runtime), (request.wrapper, request.destination), (request.runtime, request.destination)):
        if first == second or first in second.parents or second in first.parents:
            raise PackagingError("input and package output paths overlap")
    original_runtime = assembler._snapshot(request.runtime)
    original_wrapper = assembler._file_record(request.wrapper)
    stage = Path(tempfile.mkdtemp(prefix=".migration-stream-desktop-", dir=request.destination.parent))
    stage_identity = (stage.stat().st_dev, stage.stat().st_ino)
    # Failed stages are deliberately retained. In particular, never erase data
    # while an image helper or publication outcome might remain unresolved.
    image_root = stage / "image-root"
    image_root.mkdir(mode=0o700)
    staged_app = image_root / APP_NAME
    from dataclasses import replace
    reports = []
    def checked(root):
        envelope = validator(root)
        identity = assembler._report(envelope, root)
        if identity["sourceState"] != "CLEAN":
            raise PackagingError("desktop candidate requires a clean source runtime")
        reports.append(identity)
        return envelope
    assembler.assemble(replace(request, destination=staged_app), validator=checked, publisher=publisher)
    original_app = assembler._contents(assembler._snapshot(staged_app))
    image = stage / IMAGE_NAME
    image_result = image_maker(image_root, image)
    if image_result != {"format": "UDZO", "filesystem": "HFS+", "checksumVerification": "hdiutil-exit-zero"}:
        raise PackagingError("image tool result contract differs")
    _image_record(image)  # Refuse links/aliases before any metadata write.
    image_fd = os.open(image, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(image_fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise PackagingError("image identity is unsafe before sealing")
        os.fchmod(image_fd, 0o444)
    finally:
        os.close(image_fd)
    image_record = _image_record(image)
    if assembler._contents(assembler._snapshot(staged_app)) != original_app:
        raise PackagingError("application bytes or modes changed during imaging")
    app = stage / APP_NAME
    assembler._real_path(app, absent=True)
    os.rename(staged_app, app)  # Both names are exclusively inside our own private stage.
    image_root.rmdir()
    runtime = app / "Contents/Resources/Runtime"
    if _runtime_identity(validator, runtime) != reports[0]:
        raise PackagingError("prepublication runtime identity differs")
    if assembler._snapshot(request.runtime) != original_runtime or assembler._file_record(request.wrapper) != original_wrapper:
        raise PackagingError("trusted input changed during packaging")
    if assembler._contents(assembler._snapshot(app)) != original_app or _image_record(image) != image_record:
        raise PackagingError("package changed before publication")
    receipt = {
        "schemaVersion": SCHEMA, "policy": assembler.POLICY,
        "architecture": "arm64", "runtime": reports[0],
        "app": {"name": APP_NAME, "entries": original_app, "wrapperSha256": original_wrapper[3]},
        "image": {"name": IMAGE_NAME, **image_record, **image_result},
        "signingPerformed": False, "inputSignatures": "preserved; not inspected",
        "notarizationPerformed": False, "installationPerformed": False,
        "explicitImageAttachment": False, "mountedContentsQualified": False,
    }
    data = (json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n").encode()
    if len(data) > MAX_RECEIPT:
        raise PackagingError("package receipt exceeds bound")
    with (stage / RECEIPT_NAME).open("xb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    (stage / RECEIPT_NAME).chmod(0o444)
    current = stage.lstat()
    if (current.st_dev, current.st_ino) != stage_identity or not stat.S_ISDIR(current.st_mode):
        raise PackagingError("owned staging identity changed")
    _package_names(stage)
    stage.chmod(0o755)
    publisher(stage, request.destination)
    # A post-publication failure preserves visible output; never delete it.
    _package_names(request.destination)
    if assembler._file_record(request.destination / RECEIPT_NAME)[3] != hashlib.sha256(data).hexdigest():
        raise PackagingError("visible package receipt changed")
    final_app = request.destination / APP_NAME
    if _runtime_identity(validator, final_app / "Contents/Resources/Runtime") != reports[0] or assembler._contents(assembler._snapshot(final_app)) != original_app or _image_record(request.destination / IMAGE_NAME) != image_record:
        raise PackagingError("visible package freshness verification failed")
    return {"schemaVersion": SCHEMA, "ok": True, "architecture": "arm64", "image": image_record,
            "wrapperSha256": original_wrapper[3], "runtimeManifestSha256": reports[0]["manifestSha256"],
            "signingPerformed": False, "mountedContentsQualified": False}


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("wrapper", "runtime", "output-directory"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("bundle-identifier", "minimum-os", "bundle-version", "short-version"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--engineering-policy", required=True, choices=[assembler.POLICY])
    values = vars(parser.parse_args(argv))
    values["destination"] = values.pop("output_directory")
    return assembler.Request(**values)


def main():
    try:
        result = package(arguments())
    except (assembler.AssemblyError, PackagingError, OSError, ValueError, TypeError):
        result = {"schemaVersion": SCHEMA, "ok": False, "code": "PACKAGING_REFUSED"}
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
