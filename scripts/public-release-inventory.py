#!/usr/bin/env python3
"""Build and verify a non-publishing clean-export candidate from exact Git bytes."""

from __future__ import annotations

import argparse
import base64
import binascii
import fnmatch
import hashlib
import json
import os
import posixpath
import re
import stat
import subprocess
import sys
import tempfile
import threading
import tomllib
import unicodedata
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any, BinaryIO
from urllib.parse import quote, urlsplit


SCHEMA = "migration-stream/public-release-inventory/v1"
PURPOSE = "release-preparation evidence only; not publication authority"
REGULAR_MODES = {"100644", "100755"}
MAXIMUM_TRACKED_ENTRIES = 10_000
MAXIMUM_SELECTED_FILES = 5_000
MAXIMUM_SELECTED_FILE_BYTES = 64 * 1024 * 1024
MAXIMUM_SELECTED_TOTAL_BYTES = 512 * 1024 * 1024
MAXIMUM_POLICY_BYTES = 1024 * 1024
MAXIMUM_POLICY_PATTERNS = 512
MAXIMUM_PATTERN_BYTES = 512
MAXIMUM_TREE_OUTPUT_BYTES = 8 * 1024 * 1024
MAXIMUM_GIT_OUTPUT_BYTES = 64 * 1024 * 1024
MAXIMUM_GIT_DIAGNOSTIC_BYTES = 1024 * 1024
MAXIMUM_DEPENDENCY_INPUT_BYTES = 16 * 1024 * 1024
MAXIMUM_DEPENDENCY_MANIFEST_BYTES = 1024 * 1024
MAXIMUM_DEPENDENCY_COMPONENTS = 5_000
MAXIMUM_DEPENDENCY_TEXT_BYTES = 4_096
MAXIMUM_NPM_NAME_BYTES = 214
MAXIMUM_DEPENDENCY_AUTHORITY_TOTAL_BYTES = 32 * 1024 * 1024
MAXIMUM_DEPENDENCY_SBOM_BYTES = 8 * 1024 * 1024
CYCLONEDX_SCHEMA = "https://cyclonedx.org/schema/bom-1.6.schema.json"
SEMANTIC_VERSION = re.compile(
    r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\Z"
)
CARGO_NAME = re.compile(r"[0-9A-Za-z_-]+\Z")
NPM_NAME_SEGMENT = re.compile(r"[a-z0-9~-][a-z0-9._~-]*\Z")
LOWERCASE_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
CARGO_LOCK_DEPENDENCY = re.compile(
    r"(?P<name>[0-9A-Za-z_-]+)(?: (?P<version>[^ ()]+)(?: \((?P<source>[^()]+)\))?)?\Z"
)
NODE_ENGINE_CLAUSE = re.compile(
    r"(?P<operator>\^|>=)?(?P<version>(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))\Z"
)
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
WINDOWS_RESERVED_NAMES = {
    "aux",
    "clock$",
    "con",
    "conin$",
    "conout$",
    "nul",
    "prn",
    *(f"com{number}" for number in range(1, 10)),
    *(f"lpt{number}" for number in range(1, 10)),
}
WINDOWS_FORBIDDEN_CHARACTERS = set('<>:"|?*')


class InventoryError(RuntimeError):
    """A fail-closed public-release inventory error."""


def git(
    root: Path,
    *arguments: str,
    maximum_output_bytes: int = MAXIMUM_GIT_OUTPUT_BYTES,
) -> bytes:
    process = subprocess.Popen(
        ["git", "--no-replace-objects", *arguments],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdout is not None
    assert process.stderr is not None
    outputs: dict[str, bytes] = {}
    overflows: list[str] = []
    reader_errors: list[str] = []
    lock = threading.Lock()

    def drain(name: str, limit: int) -> None:
        stream = process.stdout if name == "stdout" else process.stderr
        chunks: list[bytes] = []
        observed = 0
        try:
            while True:
                chunk = stream.read(64 * 1024)
                if not chunk:
                    break
                observed += len(chunk)
                if observed <= limit:
                    chunks.append(chunk)
                else:
                    with lock:
                        if name not in overflows:
                            overflows.append(name)
                    try:
                        process.kill()
                    except OSError:
                        pass
        except OSError:
            with lock:
                reader_errors.append(name)
            try:
                process.kill()
            except OSError:
                pass
        finally:
            outputs[name] = b"".join(chunks)

    readers = [
        threading.Thread(target=drain, args=("stdout", maximum_output_bytes)),
        threading.Thread(
            target=drain,
            args=("stderr", MAXIMUM_GIT_DIAGNOSTIC_BYTES),
        ),
    ]
    for reader in readers:
        reader.start()
    return_code = process.wait()
    for reader in readers:
        reader.join()
    if overflows:
        raise InventoryError(f"Git {' and '.join(overflows)} exceeded the byte bound")
    if reader_errors:
        raise InventoryError("Git output could not be read")
    if return_code != 0:
        detail = outputs["stderr"].decode("utf-8", errors="replace").strip()
        raise InventoryError(f"Git command failed: {detail or 'no diagnostic'}")
    return outputs["stdout"]


def require_clean_repository(root: Path) -> None:
    process = subprocess.Popen(
        [
            "git",
            "--no-replace-objects",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    assert process.stdout is not None
    first = process.stdout.read(1)
    if first:
        process.kill()
        process.wait()
        raise InventoryError("repository must be clean, including untracked files")
    return_code = process.wait()
    if return_code != 0:
        raise InventoryError("Git status could not be verified")


def canonical_json(document: dict[str, Any]) -> bytes:
    return (
        json.dumps(
            document,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        )
        + "\n"
    ).encode("utf-8")


def bounded_text(value: Any, label: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or value != value.strip()
        or value != unicodedata.normalize("NFKC", value)
        or len(value.encode("utf-8")) > MAXIMUM_DEPENDENCY_TEXT_BYTES
        or any(
            unicodedata.category(character).startswith("C")
            or is_default_ignorable(character)
            for character in value
        )
    ):
        raise InventoryError(f"{label} is invalid")
    return value


def mapping(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or not all(isinstance(key, str) for key in value):
        raise InventoryError(f"{label} must be an object")
    return value


def exact_commit_blob(
    root: Path,
    commit: str,
    path: str,
    maximum_bytes: int,
) -> bytes:
    safe_relative_path(path)
    object_id = (
        git(root, "rev-parse", "--verify", f"{commit}:{path}")
        .decode("ascii")
        .strip()
    )
    object_kind = git(root, "cat-file", "-t", object_id).decode("ascii").strip()
    if object_kind != "blob":
        raise InventoryError(f"dependency authority is not a Git file: {path}")
    try:
        object_size = int(git(root, "cat-file", "-s", object_id).decode("ascii"))
    except ValueError as error:
        raise InventoryError(f"dependency authority has an invalid size: {path}") from error
    if object_size > maximum_bytes:
        raise InventoryError(f"dependency authority exceeds the byte bound: {path}")
    data = git(
        root,
        "cat-file",
        "blob",
        object_id,
        maximum_output_bytes=maximum_bytes,
    )
    if len(data) != object_size:
        raise InventoryError(f"dependency authority size changed while reading: {path}")
    return data


def parse_toml_authority(data: bytes, label: str) -> dict[str, Any]:
    try:
        document = tomllib.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise InventoryError(f"{label} is not valid UTF-8 TOML") from error
    return mapping(document, label)


def parse_json_authority(data: bytes, label: str) -> dict[str, Any]:
    def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise InventoryError(f"{label} contains a duplicate key: {key}")
            result[key] = value
        return result

    def reject_constant(value: str) -> None:
        raise InventoryError(f"{label} contains a non-finite number: {value}")

    try:
        document = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=reject_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InventoryError(f"{label} is not valid UTF-8 JSON") from error
    return mapping(document, label)


def semantic_version(value: Any, label: str) -> str:
    version = bounded_text(value, label)
    if SEMANTIC_VERSION.fullmatch(version) is None:
        raise InventoryError(f"{label} is not an exact semantic version")
    return version


def exact_cargo_pin(value: Any, label: str) -> str:
    pin = bounded_text(value, label)
    if not pin.startswith("=") or SEMANTIC_VERSION.fullmatch(pin[1:]) is None:
        raise InventoryError(f"{label} must be an exact '=version' pin")
    return pin[1:]


def exact_npm_pin(value: Any, label: str) -> str:
    pin = bounded_text(value, label)
    if SEMANTIC_VERSION.fullmatch(pin) is None:
        raise InventoryError(f"{label} must be an exact version pin")
    return pin


def npm_identity(value: Any, label: str) -> str:
    name = bounded_text(value, label)
    if len(name.encode("utf-8")) > MAXIMUM_NPM_NAME_BYTES or name != name.lower():
        raise InventoryError(f"{label} is not a canonical npm package name")
    if name.startswith("@"):
        parts = name.split("/")
        if len(parts) != 2:
            raise InventoryError(f"{label} is not a canonical npm package name")
        scope = parts[0][1:]
        package = parts[1]
        segments = (scope, package)
    else:
        if "/" in name:
            raise InventoryError(f"{label} is not a canonical npm package name")
        segments = (name,)
    if any(
        segment in {".", ".."}
        or NPM_NAME_SEGMENT.fullmatch(segment) is None
        for segment in segments
    ):
        raise InventoryError(f"{label} is not a canonical npm package name")
    return name


def node_version_pin(data: bytes) -> str:
    try:
        text = data.decode("ascii")
    except UnicodeDecodeError as error:
        raise InventoryError("web/.node-version is not ASCII") from error
    candidate = text.removesuffix("\n")
    if text != f"{candidate}\n" or "\r" in text:
        raise InventoryError("web/.node-version must contain one exact version line")
    match = NODE_ENGINE_CLAUSE.fullmatch(candidate)
    if match is None or match.group("operator") is not None:
        raise InventoryError("web/.node-version pin is not an exact x.y.z version")
    return candidate


def version_triplet(value: str) -> tuple[int, int, int]:
    major, minor, patch = value.split(".")
    return int(major), int(minor), int(patch)


def node_engine_accepts(engine: str, pin: str) -> bool:
    clauses = engine.split(" || ")
    if not clauses or any(not clause for clause in clauses):
        raise InventoryError("Node engine constraint uses unsupported grammar")
    pinned = version_triplet(pin)
    accepted = False
    for clause in clauses:
        match = NODE_ENGINE_CLAUSE.fullmatch(clause)
        if match is None:
            raise InventoryError("Node engine constraint uses unsupported grammar")
        requested = version_triplet(match.group("version"))
        operator = match.group("operator")
        if operator is None:
            clause_accepts = pinned == requested
        elif operator == ">=":
            clause_accepts = pinned >= requested
        else:
            major, minor, patch = requested
            if major > 0:
                upper = (major + 1, 0, 0)
            elif minor > 0:
                upper = (0, minor + 1, 0)
            else:
                upper = (0, 0, patch + 1)
            clause_accepts = requested <= pinned < upper
        accepted = accepted or clause_accepts
    return accepted


def authority_set_sha256(authorities: dict[str, bytes]) -> str:
    digest = hashlib.sha256()
    for path in sorted(authorities):
        encoded_path = path.encode("utf-8")
        data = authorities[path]
        digest.update(len(encoded_path).to_bytes(4, "big"))
        digest.update(encoded_path)
        digest.update(len(data).to_bytes(8, "big"))
        digest.update(data)
    return digest.hexdigest()


def component_property(name: str, value: str) -> dict[str, str]:
    return {"name": name, "value": value}


def cargo_purl(name: str, version: str) -> str:
    return f"pkg:cargo/{quote(name, safe='-._~')}@{quote(version, safe='-._~')}"


def npm_purl(name: str, version: str) -> str:
    if name.startswith("@"):
        namespace, package = name.split("/", 1)
        encoded_name = f"{quote(namespace, safe='-._~')}/{quote(package, safe='-._~')}"
    else:
        encoded_name = quote(name, safe="-._~")
    return f"pkg:npm/{encoded_name}@{quote(version, safe='-._~')}"


def authority_bom_ref(ecosystem: str, *authority: str) -> str:
    digest = hashlib.sha256(
        b"\0".join(value.encode("utf-8") for value in authority)
    ).hexdigest()
    return f"urn:migration-stream:dependency:{ecosystem}:{digest}"


def cargo_dependency_tables(
    manifest: dict[str, Any], manifest_path: str
) -> list[tuple[str, dict[str, Any]]]:
    tables: list[tuple[str, dict[str, Any]]] = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        if key in manifest:
            tables.append((f"{manifest_path} [{key}]", mapping(manifest[key], key)))
    targets = manifest.get("target", {})
    if targets is not None:
        for target, target_table in mapping(targets, f"{manifest_path} [target]").items():
            bounded_text(target, f"{manifest_path} target name")
            target_mapping = mapping(target_table, f"{manifest_path} target {target}")
            for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                if key in target_mapping:
                    tables.append(
                        (
                            f"{manifest_path} [target.{target}.{key}]",
                            mapping(target_mapping[key], key),
                        )
                    )
    return tables


def cargo_manifest_identity(
    manifest: dict[str, Any], manifest_path: str, toolchain: str
) -> tuple[str, str]:
    package = mapping(manifest.get("package"), f"{manifest_path} [package]")
    name = bounded_text(package.get("name"), f"{manifest_path} package name")
    if CARGO_NAME.fullmatch(name) is None:
        raise InventoryError(f"{manifest_path} package name is invalid")
    version = semantic_version(package.get("version"), f"{manifest_path} package version")
    rust_version = semantic_version(
        package.get("rust-version"), f"{manifest_path} rust-version"
    )
    if rust_version != toolchain:
        raise InventoryError(f"{manifest_path} rust-version does not match the toolchain")
    return name, version


def normalized_cargo_path(manifest_path: str, dependency_path: str) -> str:
    bounded_text(dependency_path, f"{manifest_path} dependency path")
    if "\\" in dependency_path or dependency_path.startswith("/"):
        raise InventoryError(f"{manifest_path} dependency path is unsafe")
    manifest_directory = str(PurePosixPath(manifest_path).parent)
    combined = posixpath.normpath(posixpath.join(manifest_directory, dependency_path))
    if combined in {"", ".", ".."} or combined.startswith("../"):
        raise InventoryError(f"{manifest_path} dependency path is unsafe")
    safe_relative_path(combined)
    return f"{combined.rstrip('/')}/Cargo.toml"


def validate_cargo_direct_pins(
    manifests: dict[str, dict[str, Any]],
    identities: dict[str, tuple[str, str]],
    external_packages: dict[tuple[str, str], dict[str, Any]],
    external_bom_refs: dict[tuple[str, str], str],
    local_bom_refs: dict[tuple[str, str], str],
) -> tuple[dict[str, list[str]], int, set[str]]:
    declarations: dict[str, list[str]] = {path: [] for path in manifests}
    external_declaration_count = 0
    direct_external_refs: set[str] = set()
    for manifest_path, manifest in manifests.items():
        for table_label, dependencies in cargo_dependency_tables(
            manifest, manifest_path
        ):
            for dependency_alias, raw_specification in dependencies.items():
                alias = bounded_text(dependency_alias, f"{table_label} dependency name")
                if CARGO_NAME.fullmatch(alias) is None:
                    raise InventoryError(f"{table_label} dependency name is invalid")
                if isinstance(raw_specification, str):
                    specification: dict[str, Any] = {"version": raw_specification}
                else:
                    specification = mapping(
                        raw_specification, f"{table_label} dependency {alias}"
                    )
                actual_name = bounded_text(
                    specification.get("package", alias),
                    f"{table_label} dependency package",
                )
                if CARGO_NAME.fullmatch(actual_name) is None:
                    raise InventoryError(f"{table_label} dependency package is invalid")
                if specification.get("workspace") is not None:
                    raise InventoryError(
                        f"{table_label} dependency {alias} uses unsupported workspace inheritance"
                    )
                if "path" in specification:
                    if "git" in specification or "registry" in specification:
                        raise InventoryError(
                            f"{table_label} dependency {alias} mixes path and remote authority"
                        )
                    target_manifest = normalized_cargo_path(
                        manifest_path,
                        bounded_text(
                            specification["path"],
                            f"{table_label} dependency {alias} path",
                        ),
                    )
                    target_identity = identities.get(target_manifest)
                    if target_identity is None or target_identity[0] != actual_name:
                        raise InventoryError(
                            f"{table_label} path dependency {alias} is not a workspace member"
                        )
                    if "version" in specification:
                        requested = exact_cargo_pin(
                            specification["version"],
                            f"{table_label} dependency {alias}",
                        )
                        if requested != target_identity[1]:
                            raise InventoryError(
                                f"{table_label} path dependency {alias} version drifted"
                            )
                    declarations[manifest_path].append(local_bom_refs[target_identity])
                    continue
                if "git" in specification or "registry" in specification:
                    raise InventoryError(
                        f"{table_label} dependency {alias} has unsupported remote authority"
                    )
                requested = exact_cargo_pin(
                    specification.get("version"),
                    f"{table_label} dependency {alias}",
                )
                if (actual_name, requested) not in external_packages:
                    raise InventoryError(
                        f"{table_label} dependency {alias} does not match Cargo.lock"
                    )
                reference = external_bom_refs[(actual_name, requested)]
                declarations[manifest_path].append(reference)
                external_declaration_count += 1
                direct_external_refs.add(reference)
    return declarations, external_declaration_count, direct_external_refs


def resolve_cargo_dependency(
    value: str,
    authorities: dict[tuple[str, str, str], dict[str, Any]],
    references: dict[tuple[str, str, str], str],
) -> str:
    dependency = bounded_text(value, "Cargo.lock dependency reference")
    match = CARGO_LOCK_DEPENDENCY.fullmatch(dependency)
    if match is None:
        raise InventoryError(f"Cargo.lock dependency reference is malformed: {dependency}")
    name = match.group("name")
    version = match.group("version")
    source = match.group("source")
    if version is not None:
        semantic_version(version, f"Cargo.lock dependency {name} version")
    candidates = [
        authority
        for authority in authorities
        if authority[0] == name
        and (version is None or authority[1] == version)
        and (source is None or authority[2] == source)
    ]
    if len(candidates) != 1:
        raise InventoryError(
            f"Cargo.lock dependency reference is unresolved or ambiguous: {dependency}"
        )
    return references[candidates[0]]


def npm_name_from_lock_path(path: str) -> str:
    bounded_text(path, "npm lock package path")
    if "\\" in path or not path.startswith("node_modules/"):
        raise InventoryError(f"npm lock package path is invalid: {path}")
    parts = path.split("/")
    index = 0
    observed_names: list[str] = []
    while index < len(parts):
        if parts[index] != "node_modules":
            raise InventoryError(f"npm lock package path is invalid: {path}")
        index += 1
        if index >= len(parts):
            raise InventoryError(f"npm lock package path is invalid: {path}")
        if parts[index].startswith("@"):
            if index + 1 >= len(parts):
                raise InventoryError(f"npm lock package path is invalid: {path}")
            name = f"{parts[index]}/{parts[index + 1]}"
            index += 2
        else:
            name = parts[index]
            index += 1
        observed_names.append(npm_identity(name, f"npm lock package identity {path}"))
    return observed_names[-1]


def npm_integrity_hash(value: Any, path: str) -> str:
    integrity = bounded_text(value, f"npm integrity for {path}")
    if not integrity.startswith("sha512-") or " " in integrity:
        raise InventoryError(f"npm integrity is unsupported or malformed: {path}")
    encoded = integrity.removeprefix("sha512-")
    try:
        digest = base64.b64decode(encoded, validate=True)
    except (ValueError, binascii.Error) as error:
        raise InventoryError(f"npm integrity is malformed: {path}") from error
    if len(digest) != 64 or base64.b64encode(digest).decode("ascii") != encoded:
        raise InventoryError(f"npm integrity is not canonical SHA-512: {path}")
    return digest.hex()


def validate_string_map(value: Any, label: str) -> dict[str, str]:
    entries = mapping(value, label)
    result: dict[str, str] = {}
    for name, version in entries.items():
        package_name = npm_identity(name, f"{label} package name")
        result[package_name] = bounded_text(version, f"{label} requirement")
    return result


def build_dependency_sbom(
    root: Path, inventory: dict[str, Any]
) -> dict[str, Any]:
    root = root.resolve(strict=True)
    require_clean_repository(root)
    if inventory.get("schema") != SCHEMA:
        raise InventoryError("dependency SBOM requires a public-release inventory")
    commit = bounded_text(inventory.get("sourceCommit"), "inventory source commit")
    source_tree = bounded_text(inventory.get("sourceTree"), "inventory source tree")
    current_commit = (
        git(root, "rev-parse", "--verify", "HEAD^{commit}").decode("ascii").strip()
    )
    if current_commit != commit:
        raise InventoryError("dependency SBOM source commit does not match HEAD")
    current_tree = (
        git(root, "rev-parse", "--verify", f"{commit}^{{tree}}")
        .decode("ascii")
        .strip()
    )
    if current_tree != source_tree:
        raise InventoryError("dependency SBOM source tree does not match the inventory")

    cargo_lock_bytes = exact_commit_blob(
        root, commit, "Cargo.lock", MAXIMUM_DEPENDENCY_INPUT_BYTES
    )
    cargo_manifest_bytes = exact_commit_blob(
        root, commit, "Cargo.toml", MAXIMUM_DEPENDENCY_MANIFEST_BYTES
    )
    toolchain_bytes = exact_commit_blob(
        root, commit, "rust-toolchain.toml", MAXIMUM_DEPENDENCY_MANIFEST_BYTES
    )
    npm_manifest_bytes = exact_commit_blob(
        root, commit, "web/package.json", MAXIMUM_DEPENDENCY_MANIFEST_BYTES
    )
    npm_lock_bytes = exact_commit_blob(
        root, commit, "web/package-lock.json", MAXIMUM_DEPENDENCY_INPUT_BYTES
    )
    node_version_bytes = exact_commit_blob(
        root, commit, "web/.node-version", MAXIMUM_DEPENDENCY_MANIFEST_BYTES
    )
    corpus_policy_bytes = exact_commit_blob(
        root,
        commit,
        "config/release-corpus.json",
        MAXIMUM_DEPENDENCY_MANIFEST_BYTES,
    )
    corpus_policy_sha256 = hashlib.sha256(corpus_policy_bytes).hexdigest()
    recorded_corpus_sha256 = bounded_text(
        inventory.get("corpusPolicySha256"), "inventory corpus policy SHA-256"
    )
    if (
        LOWERCASE_SHA256.fullmatch(recorded_corpus_sha256) is None
        or recorded_corpus_sha256 != corpus_policy_sha256
    ):
        raise InventoryError("inventory corpus policy binding is invalid")
    inventory_bytes = canonical_json(inventory)
    inventory_sha256 = hashlib.sha256(inventory_bytes).hexdigest()
    authority_total_bytes = sum(
        len(data)
        for data in (
            cargo_lock_bytes,
            cargo_manifest_bytes,
            toolchain_bytes,
            npm_manifest_bytes,
            npm_lock_bytes,
            node_version_bytes,
        )
    )
    if authority_total_bytes > MAXIMUM_DEPENDENCY_AUTHORITY_TOTAL_BYTES:
        raise InventoryError("dependency authority set exceeds the total-byte bound")

    cargo_lock = parse_toml_authority(cargo_lock_bytes, "Cargo.lock")
    root_manifest = parse_toml_authority(cargo_manifest_bytes, "Cargo.toml")
    toolchain_document = parse_toml_authority(
        toolchain_bytes, "rust-toolchain.toml"
    )
    npm_manifest = parse_json_authority(npm_manifest_bytes, "web/package.json")
    npm_lock = parse_json_authority(npm_lock_bytes, "web/package-lock.json")
    node_pin = node_version_pin(node_version_bytes)

    toolchain_table = mapping(
        toolchain_document.get("toolchain"), "rust-toolchain.toml [toolchain]"
    )
    toolchain = semantic_version(
        toolchain_table.get("channel"), "rust-toolchain.toml channel"
    )
    workspace = mapping(root_manifest.get("workspace"), "Cargo.toml [workspace]")
    raw_members = workspace.get("members")
    if (
        not isinstance(raw_members, list)
        or not raw_members
        or len(raw_members) > MAXIMUM_DEPENDENCY_COMPONENTS
    ):
        raise InventoryError("Cargo workspace members are invalid")
    member_paths: list[str] = []
    for raw_member in raw_members:
        member = bounded_text(raw_member, "Cargo workspace member")
        safe_relative_path(member)
        member_manifest = f"{member.rstrip('/')}/Cargo.toml"
        safe_relative_path(member_manifest)
        if member_manifest in member_paths:
            raise InventoryError("Cargo workspace contains a duplicate member")
        member_paths.append(member_manifest)

    manifests: dict[str, dict[str, Any]] = {"Cargo.toml": root_manifest}
    manifest_bytes: dict[str, bytes] = {"Cargo.toml": cargo_manifest_bytes}
    for member_manifest in member_paths:
        data = exact_commit_blob(
            root, commit, member_manifest, MAXIMUM_DEPENDENCY_MANIFEST_BYTES
        )
        authority_total_bytes += len(data)
        if authority_total_bytes > MAXIMUM_DEPENDENCY_AUTHORITY_TOTAL_BYTES:
            raise InventoryError("dependency authority set exceeds the total-byte bound")
        manifests[member_manifest] = parse_toml_authority(data, member_manifest)
        manifest_bytes[member_manifest] = data
    authority_bytes = {
        "Cargo.lock": cargo_lock_bytes,
        "Cargo.toml": cargo_manifest_bytes,
        "rust-toolchain.toml": toolchain_bytes,
        "web/.node-version": node_version_bytes,
        "web/package-lock.json": npm_lock_bytes,
        "web/package.json": npm_manifest_bytes,
        **{
            path: data
            for path, data in manifest_bytes.items()
            if path != "Cargo.toml"
        },
    }
    if sum(len(data) for data in authority_bytes.values()) != authority_total_bytes:
        raise InventoryError("dependency authority-set byte accounting drifted")
    authority_sha256 = authority_set_sha256(authority_bytes)
    identities = {
        path: cargo_manifest_identity(manifest, path, toolchain)
        for path, manifest in manifests.items()
    }
    if len(set(identities.values())) != len(identities):
        raise InventoryError("Cargo workspace package identities are not unique")
    required_public_paths = {
        "Cargo.lock",
        "Cargo.toml",
        "rust-toolchain.toml",
        "web/.node-version",
        "web/package.json",
        "web/package-lock.json",
        "config/release-corpus.json",
        *member_paths,
    }
    try:
        selected_public_paths = {record["path"] for record in inventory["files"]}
    except (KeyError, TypeError) as error:
        raise InventoryError("public-release inventory file authority is invalid") from error
    missing_public_paths = sorted(required_public_paths - selected_public_paths)
    if missing_public_paths:
        raise InventoryError(
            "dependency SBOM authority is outside the selected public corpus: "
            + ", ".join(missing_public_paths)
        )

    if cargo_lock.get("version") != 4:
        raise InventoryError("Cargo.lock must use lockfile version 4")
    raw_cargo_packages = cargo_lock.get("package")
    if (
        not isinstance(raw_cargo_packages, list)
        or not raw_cargo_packages
        or len(raw_cargo_packages) > MAXIMUM_DEPENDENCY_COMPONENTS
    ):
        raise InventoryError("Cargo.lock package universe is invalid")
    cargo_components: list[dict[str, Any]] = []
    local_lock_identities: set[tuple[str, str]] = set()
    local_lock_packages: dict[tuple[str, str], dict[str, Any]] = {}
    external_packages: dict[tuple[str, str], dict[str, Any]] = {}
    external_bom_refs: dict[tuple[str, str], str] = {}
    local_bom_refs = {
        identity: authority_bom_ref("cargo-workspace", path, *identity)
        for path, identity in identities.items()
    }
    cargo_authorities: dict[tuple[str, str, str], dict[str, Any]] = {}
    cargo_bom_refs: dict[tuple[str, str, str], str] = {}
    for index, raw_package in enumerate(raw_cargo_packages):
        package = mapping(raw_package, f"Cargo.lock package {index}")
        name = bounded_text(package.get("name"), f"Cargo.lock package {index} name")
        if CARGO_NAME.fullmatch(name) is None:
            raise InventoryError(f"Cargo.lock package {index} name is invalid")
        version = semantic_version(
            package.get("version"), f"Cargo.lock package {name} version"
        )
        dependencies = package.get("dependencies", [])
        if not isinstance(dependencies, list) or not all(
            isinstance(dependency, str)
            and dependency
            and len(dependency.encode("utf-8")) <= MAXIMUM_DEPENDENCY_TEXT_BYTES
            for dependency in dependencies
        ):
            raise InventoryError(f"Cargo.lock package {name} dependencies are invalid")
        source = package.get("source")
        checksum = package.get("checksum")
        if source is None:
            if checksum is not None:
                raise InventoryError(f"local Cargo package {name} has a checksum")
            identity = (name, version)
            if identity in local_lock_identities:
                raise InventoryError("Cargo.lock has a duplicate local package identity")
            local_lock_identities.add(identity)
            if identity not in local_bom_refs:
                raise InventoryError(
                    f"Cargo.lock contains unknown source-less package: {name} {version}"
                )
            authority = (name, version, "")
            cargo_authorities[authority] = package
            cargo_bom_refs[authority] = local_bom_refs[identity]
            local_lock_packages[identity] = package
            continue
        source_text = bounded_text(source, f"Cargo.lock package {name} source")
        if source_text != CRATES_IO_SOURCE:
            raise InventoryError(f"Cargo.lock package {name} has an unsupported source")
        checksum_text = bounded_text(
            checksum, f"Cargo.lock package {name} checksum"
        )
        if LOWERCASE_SHA256.fullmatch(checksum_text) is None:
            raise InventoryError(f"Cargo.lock package {name} checksum is invalid")
        authority = (name, version, source_text)
        if authority in cargo_authorities:
            raise InventoryError("Cargo.lock has a duplicate package authority")
        cargo_authorities[authority] = package
        identity = (name, version)
        if identity in external_packages:
            raise InventoryError(
                "Cargo.lock has an ambiguous name/version across source authorities"
            )
        purl = cargo_purl(name, version)
        bom_ref = authority_bom_ref("cargo", source_text, name, version)
        cargo_bom_refs[authority] = bom_ref
        component = {
            "bom-ref": bom_ref,
            "hashes": [{"alg": "SHA-256", "content": checksum_text}],
            "name": name,
            "properties": [
                component_property("migration-stream:evidence:lockfile", "Cargo.lock"),
                component_property("migration-stream:evidence:source", source_text),
            ],
            "purl": purl,
            "type": "library",
            "version": version,
        }
        external_packages[identity] = package
        external_bom_refs[identity] = bom_ref
        cargo_components.append(component)

    expected_local_identities = set(identities.values())
    if local_lock_identities != expected_local_identities:
        raise InventoryError("Cargo.lock local package identities drifted from the workspace")
    (
        cargo_declarations,
        cargo_direct_declaration_count,
        cargo_direct_refs,
    ) = validate_cargo_direct_pins(
        manifests,
        identities,
        external_packages,
        external_bom_refs,
        local_bom_refs,
    )
    for manifest_path, declared_references in cargo_declarations.items():
        identity = identities[manifest_path]
        locked_dependency_values = local_lock_packages[identity].get("dependencies", [])
        locked_references = {
            resolve_cargo_dependency(
                dependency, cargo_authorities, cargo_bom_refs
            )
            for dependency in locked_dependency_values
        }
        if len(locked_references) != len(locked_dependency_values):
            raise InventoryError(
                f"{manifest_path} has duplicate direct dependencies in Cargo.lock"
            )
        if set(declared_references) != locked_references:
            raise InventoryError(
                f"{manifest_path} direct dependencies drifted from Cargo.lock"
            )

    if npm_lock.get("lockfileVersion") != 3 or npm_lock.get("requires") is not True:
        raise InventoryError("web/package-lock.json must use complete lockfile version 3")
    npm_root_name = npm_identity(npm_manifest.get("name"), "web/package.json name")
    npm_root_version = semantic_version(
        npm_manifest.get("version"), "web/package.json version"
    )
    raw_npm_packages = mapping(
        npm_lock.get("packages"), "web/package-lock.json packages"
    )
    npm_root = mapping(
        raw_npm_packages.get(""), "web/package-lock.json root package"
    )
    if (
        npm_lock.get("name") != npm_root_name
        or npm_lock.get("version") != npm_root_version
        or npm_root.get("name") != npm_root_name
        or npm_root.get("version") != npm_root_version
    ):
        raise InventoryError("npm lock root identity drifted from web/package.json")
    manifest_engines = validate_string_map(
        npm_manifest.get("engines", {}), "web/package.json engines"
    )
    lock_engines = validate_string_map(
        npm_root.get("engines", {}), "web/package-lock.json root engines"
    )
    if manifest_engines != lock_engines or set(manifest_engines) != {"node"}:
        raise InventoryError("npm root engines drifted or lack one Node constraint")
    node_engine = manifest_engines["node"]
    if not node_engine_accepts(node_engine, node_pin):
        raise InventoryError("web/.node-version does not satisfy the Node engine")
    for unsupported_kind in ("peerDependencies", "peerDependenciesMeta"):
        manifest_unsupported = mapping(
            npm_manifest.get(unsupported_kind, {}),
            f"web/package.json {unsupported_kind}",
        )
        lock_unsupported = mapping(
            npm_root.get(unsupported_kind, {}),
            f"web/package-lock.json root {unsupported_kind}",
        )
        if manifest_unsupported or lock_unsupported:
            raise InventoryError(f"npm root {unsupported_kind} is unsupported")
    npm_direct_paths: list[str] = []
    npm_direct_declaration_count = 0
    for dependency_kind in (
        "dependencies",
        "devDependencies",
        "optionalDependencies",
    ):
        manifest_dependencies = validate_string_map(
            npm_manifest.get(dependency_kind, {}),
            f"web/package.json {dependency_kind}",
        )
        lock_dependencies = validate_string_map(
            npm_root.get(dependency_kind, {}),
            f"web/package-lock.json root {dependency_kind}",
        )
        if manifest_dependencies != lock_dependencies:
            raise InventoryError(f"npm root {dependency_kind} drifted from package.json")
        for dependency_name, raw_pin in manifest_dependencies.items():
            pin = exact_npm_pin(
                raw_pin, f"web/package.json {dependency_kind} {dependency_name}"
            )
            locked = raw_npm_packages.get(f"node_modules/{dependency_name}")
            if not isinstance(locked, dict) or locked.get("version") != pin:
                raise InventoryError(
                    f"npm direct dependency {dependency_name} does not match the lockfile"
                )
            npm_direct_paths.append(f"node_modules/{dependency_name}")
            npm_direct_declaration_count += 1

    npm_components: list[dict[str, Any]] = []
    npm_bom_refs: dict[str, str] = {}
    npm_identities: dict[str, tuple[tuple[Any, ...], dict[str, Any], list[str]]] = {}
    npm_installation_count = len(raw_npm_packages) - 1
    if npm_installation_count + len(cargo_components) > MAXIMUM_DEPENDENCY_COMPONENTS:
        raise InventoryError("locked dependency universe exceeds the component bound")
    for lock_path, raw_package in sorted(raw_npm_packages.items()):
        if lock_path == "":
            continue
        if len(npm_components) + len(cargo_components) >= MAXIMUM_DEPENDENCY_COMPONENTS:
            raise InventoryError("locked dependency universe exceeds the component bound")
        package = mapping(raw_package, f"npm lock package {lock_path}")
        if package.get("link") is not None:
            raise InventoryError(f"npm link package is unsupported: {lock_path}")
        name = npm_name_from_lock_path(lock_path)
        if "name" in package and npm_identity(
            package["name"], f"npm explicit package name {lock_path}"
        ) != name:
            raise InventoryError(f"npm package identity drifted from its path: {lock_path}")
        version = semantic_version(package.get("version"), f"npm version for {lock_path}")
        resolved = bounded_text(package.get("resolved"), f"npm resolved URL for {lock_path}")
        try:
            parsed_resolved = urlsplit(resolved)
        except ValueError as error:
            raise InventoryError(f"npm resolved URL is malformed: {lock_path}") from error
        if (
            parsed_resolved.scheme != "https"
            or parsed_resolved.netloc != "registry.npmjs.org"
            or parsed_resolved.username is not None
            or parsed_resolved.password is not None
            or parsed_resolved.query
            or parsed_resolved.fragment
        ):
            raise InventoryError(f"npm resolved URL is invalid: {lock_path}")
        integrity_hash = npm_integrity_hash(package.get("integrity"), lock_path)
        purl = npm_purl(name, version)
        bom_ref = authority_bom_ref("npm", lock_path, name, version, resolved)
        properties = [
            component_property(
                "migration-stream:evidence:install-path", lock_path
            ),
            component_property(
                "migration-stream:evidence:lockfile", "web/package-lock.json"
            ),
            component_property("migration-stream:evidence:resolved", resolved),
        ]
        component: dict[str, Any] = {
            "bom-ref": bom_ref,
            "hashes": [{"alg": "SHA-512", "content": integrity_hash}],
            "name": name,
            "properties": properties,
            "purl": purl,
            "type": "library",
            "version": version,
        }
        if "license" in package:
            license_text = bounded_text(
                package["license"], f"npm license string for {lock_path}"
            )
            properties.append(
                component_property(
                    "migration-stream:evidence:unreviewed-npm-lockfile-license-string",
                    license_text,
                )
            )
        identity_evidence = (name, version, resolved, integrity_hash, package.get("license"))
        existing = npm_identities.get(purl)
        if existing is not None:
            previous_evidence, previous_component, installation_paths = existing
            if identity_evidence != previous_evidence:
                raise InventoryError(
                    f"npm package identity has conflicting provenance: {name}@{version}"
                )
            installation_paths.append(lock_path)
            npm_bom_refs[lock_path] = previous_component["bom-ref"]
            continue
        npm_bom_refs[lock_path] = bom_ref
        npm_components.append(component)
        npm_identities[purl] = (identity_evidence, component, [lock_path])

    for _identity_evidence, component, installation_paths in npm_identities.values():
        if len(installation_paths) > 1:
            component["properties"].append(component_property(
                "migration-stream:evidence:install-paths",
                json.dumps(installation_paths, ensure_ascii=False, separators=(",", ":")),
            ))

    npm_direct_refs = {npm_bom_refs[path] for path in npm_direct_paths}

    components = sorted(
        cargo_components + npm_components, key=lambda component: component["bom-ref"]
    )
    if len({component["bom-ref"] for component in components}) != len(components):
        raise InventoryError("locked dependency universe has a duplicate BOM reference")
    if len({component["purl"] for component in components}) != len(components):
        raise InventoryError("locked dependency universe has a duplicate package URL")

    root_name, root_version = identities["Cargo.toml"]
    root_purl = cargo_purl(root_name, root_version)
    root_bom_ref = local_bom_refs[(root_name, root_version)]
    direct_refs = cargo_direct_refs | npm_direct_refs
    direct_declaration_count = (
        cargo_direct_declaration_count + npm_direct_declaration_count
    )
    metadata_properties = [
        component_property(
            "migration-stream:evidence:cargo-component-count",
            str(len(cargo_components)),
        ),
        component_property(
            "migration-stream:evidence:cargo-lock-sha256",
            hashlib.sha256(cargo_lock_bytes).hexdigest(),
        ),
        component_property(
            "migration-stream:evidence:authority-set-sha256", authority_sha256
        ),
        component_property(
            "migration-stream:evidence:authority-total-bytes",
            str(authority_total_bytes),
        ),
        component_property(
            "migration-stream:evidence:component-count", str(len(components))
        ),
        component_property(
            "migration-stream:evidence:direct-pin-policy",
            "exact manifest pins validated against exact lockfile bytes",
        ),
        component_property(
            "migration-stream:evidence:direct-declaration-count",
            str(direct_declaration_count),
        ),
        component_property(
            "migration-stream:evidence:direct-component-count", str(len(direct_refs))
        ),
        component_property(
            "migration-stream:evidence:inventory-sha256", inventory_sha256
        ),
        component_property(
            "migration-stream:evidence:node-engine", node_engine
        ),
        component_property(
            "migration-stream:evidence:node-pin", node_pin
        ),
        component_property(
            "migration-stream:evidence:node-version-sha256",
            hashlib.sha256(node_version_bytes).hexdigest(),
        ),
        component_property(
            "migration-stream:evidence:npm-component-count",
            str(len(npm_components)),
        ),
        component_property(
            "migration-stream:evidence:npm-installation-count", str(npm_installation_count)
        ),
        component_property(
            "migration-stream:evidence:npm-lock-sha256",
            hashlib.sha256(npm_lock_bytes).hexdigest(),
        ),
        component_property(
            "migration-stream:evidence:corpus-policy-sha256",
            corpus_policy_sha256,
        ),
        component_property("migration-stream:evidence:purpose", PURPOSE),
        component_property("migration-stream:evidence:rust-toolchain", toolchain),
        component_property("migration-stream:evidence:source-commit", commit),
        component_property("migration-stream:evidence:source-tree", source_tree),
        component_property("migration-stream:evidence:web-root-name", npm_root_name),
        component_property(
            "migration-stream:evidence:web-root-version", npm_root_version
        ),
        component_property(
            "migration-stream:evidence:workspace-manifest-sha256",
            hashlib.sha256(
                b"".join(
                    path.encode("utf-8")
                    + b"\0"
                    + hashlib.sha256(manifest_bytes[path]).digest()
                    for path in sorted(manifest_bytes)
                )
            ).hexdigest(),
        ),
    ]
    workspace_components = []
    for manifest_path, (name, version) in sorted(identities.items()):
        if manifest_path == "Cargo.toml":
            continue
        workspace_components.append(
            {
                "bom-ref": local_bom_refs[(name, version)],
                "name": name,
                "properties": [
                    component_property(
                        "migration-stream:evidence:workspace-manifest", manifest_path
                    )
                ],
                "purl": cargo_purl(name, version),
                "type": "library",
                "version": version,
            }
        )
    root_component: dict[str, Any] = {
        "bom-ref": root_bom_ref,
        "name": root_name,
        "purl": root_purl,
        "type": "application",
        "version": root_version,
    }
    if workspace_components:
        root_component["components"] = workspace_components
    all_purls = [
        root_component["purl"],
        *(component["purl"] for component in workspace_components),
        *(component["purl"] for component in components),
    ]
    if len(set(all_purls)) != len(all_purls):
        raise InventoryError(
            "root, workspace and dependency package URLs must be globally unique"
        )
    sbom = {
        "$schema": CYCLONEDX_SCHEMA,
        "bomFormat": "CycloneDX",
        "components": components,
        "metadata": {
            "component": root_component,
            "properties": sorted(
                metadata_properties, key=lambda property_: property_["name"]
            ),
        },
        "specVersion": "1.6",
        "version": 1,
    }

    if len(canonical_json(sbom)) > MAXIMUM_DEPENDENCY_SBOM_BYTES:
        raise InventoryError("dependency SBOM exceeds the canonical output-byte bound")

    require_clean_repository(root)
    replayed_commit = (
        git(root, "rev-parse", "--verify", "HEAD^{commit}").decode("ascii").strip()
    )
    if replayed_commit != commit:
        raise InventoryError("HEAD changed while the dependency SBOM was being captured")
    return sbom


def is_default_ignorable(character: str) -> bool:
    codepoint = ord(character)
    return (
        codepoint
        in {
            0x034F,
            0x061C,
            0x115F,
            0x1160,
            0x17B4,
            0x17B5,
            0x3164,
            0xFEFF,
            0xFFA0,
            0xE0000,
            0xE0001,
        }
        or 0x180B <= codepoint <= 0x180F
        or 0x200B <= codepoint <= 0x200F
        or 0x202A <= codepoint <= 0x202E
        or 0x2060 <= codepoint <= 0x206F
        or 0xFE00 <= codepoint <= 0xFE0F
        or 0x1BCA0 <= codepoint <= 0x1BCA3
        or 0x1D173 <= codepoint <= 0x1D17A
        or 0xE0020 <= codepoint <= 0xE007F
    )


def safe_relative_path(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if (
        not value
        or "\\" in value
        or path.is_absolute()
        or any(part in {"", ".", ".."} for part in path.parts)
        or len(value.encode("utf-8")) > 1024
    ):
        raise InventoryError(f"unsafe tracked path: {value!r}")
    for part in path.parts:
        folded_base = part.split(".", 1)[0].rstrip(" ").casefold()
        if (
            part != unicodedata.normalize("NFKC", part)
            or part.endswith((" ", "."))
            or len(part.encode("utf-8")) > 255
            or folded_base in WINDOWS_RESERVED_NAMES
            or any(character in WINDOWS_FORBIDDEN_CHARACTERS for character in part)
            or any(
                unicodedata.category(character).startswith("C")
                or is_default_ignorable(character)
                for character in part
            )
        ):
            raise InventoryError(f"non-portable tracked path: {value!r}")
    return path


def portable_path_key(value: str) -> str:
    path = safe_relative_path(value)
    return "/".join(part.casefold() for part in path.parts)


def register_portable_path(
    authority: dict[tuple[str, ...], tuple[str, str]], value: str
) -> None:
    path = safe_relative_path(value)
    normalized: list[str] = []
    exact: list[str] = []
    for index, part in enumerate(path.parts):
        normalized.append(part.casefold())
        exact.append(part)
        key = tuple(normalized)
        spelling = "/".join(exact)
        kind = "file" if index == len(path.parts) - 1 else "directory"
        previous = authority.get(key)
        if previous is not None and previous != (spelling, kind):
            raise InventoryError(
                "release corpus contains a portable-path collision: "
                f"{previous[0]!r} ({previous[1]}) and {spelling!r} ({kind})"
            )
        authority[key] = (spelling, kind)


def selected(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def commit_entries(root: Path, commit: str) -> list[tuple[str, str, str, str]]:
    entries: list[tuple[str, str, str, str]] = []
    process = subprocess.Popen(
        ["git", "--no-replace-objects", "ls-tree", "-rz", "--full-tree", commit],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    assert process.stdout is not None
    chunks: list[bytes] = []
    observed_bytes = 0
    while True:
        chunk = process.stdout.read(64 * 1024)
        if not chunk:
            break
        observed_bytes += len(chunk)
        if observed_bytes > MAXIMUM_TREE_OUTPUT_BYTES:
            process.kill()
            process.wait()
            raise InventoryError("Git tree output exceeds the byte bound")
        chunks.append(chunk)
    if process.wait() != 0:
        raise InventoryError("Git tree could not be read")
    for raw in b"".join(chunks).split(b"\0"):
        if not raw:
            continue
        try:
            authority, encoded_path = raw.split(b"\t", 1)
            mode, kind, object_id = authority.decode("ascii").split(" ", 2)
            path = encoded_path.decode("utf-8")
        except (UnicodeDecodeError, ValueError) as error:
            raise InventoryError("Git tree contains an invalid entry") from error
        safe_relative_path(path)
        entries.append((path, mode, kind, object_id))
        if len(entries) > MAXIMUM_TRACKED_ENTRIES:
            raise InventoryError("Git tree exceeds the tracked-entry bound")
    return sorted(entries, key=lambda entry: entry[0].encode("utf-8"))


def require_external_destination(path: Path, root: Path, label: str) -> Path:
    expanded = path.expanduser()
    if not expanded.is_absolute():
        raise InventoryError(f"{label} must be an absolute path")
    try:
        validated_name = safe_relative_path(expanded.name)
    except InventoryError as error:
        raise InventoryError(f"{label} filename must be portable") from error
    if len(validated_name.parts) != 1:
        raise InventoryError(f"{label} filename must be portable")
    if expanded.exists() or expanded.is_symlink():
        raise InventoryError(f"{label} destination must not already exist")
    try:
        parent = expanded.parent.resolve(strict=True)
    except OSError as error:
        raise InventoryError(f"{label} parent must already exist") from error
    if not parent.is_dir() or parent.is_symlink():
        raise InventoryError(f"{label} parent must be a real directory")
    ancestor = parent
    while True:
        if os.path.samefile(ancestor, root):
            raise InventoryError(f"{label} must be outside the repository")
        if ancestor.parent == ancestor:
            break
        ancestor = ancestor.parent
    return parent / expanded.name


def build_inventory(root: Path) -> tuple[dict[str, Any], dict[str, str]]:
    root = root.resolve(strict=True)
    top_level = Path(
        git(root, "rev-parse", "--show-toplevel").decode("utf-8").strip()
    ).resolve(strict=True)
    if top_level != root:
        raise InventoryError("repository root is not the Git worktree root")
    require_clean_repository(root)

    commit = git(root, "rev-parse", "--verify", "HEAD^{commit}").decode("ascii").strip()
    tree = (
        git(root, "rev-parse", "--verify", f"{commit}^{{tree}}")
        .decode("ascii")
        .strip()
    )
    corpus_object = (
        git(root, "rev-parse", "--verify", f"{commit}:config/release-corpus.json")
        .decode("ascii")
        .strip()
    )
    try:
        corpus_size = int(
            git(root, "cat-file", "-s", corpus_object).decode("ascii").strip()
        )
    except ValueError as error:
        raise InventoryError("release-corpus policy has an invalid size") from error
    if corpus_size > MAXIMUM_POLICY_BYTES:
        raise InventoryError("release-corpus policy exceeds the byte bound")
    corpus_bytes = git(
        root,
        "cat-file",
        "blob",
        corpus_object,
        maximum_output_bytes=MAXIMUM_POLICY_BYTES,
    )
    if len(corpus_bytes) != corpus_size:
        raise InventoryError("release-corpus policy size changed while reading")
    try:
        corpus = json.loads(corpus_bytes)
        include = corpus["include"]
        excluded = corpus["excluded"]
    except (KeyError, TypeError, json.JSONDecodeError) as error:
        raise InventoryError("release-corpus policy is invalid") from error
    if (
        corpus.get("schemaVersion") != 1
        or not isinstance(include, list)
        or not include
        or not isinstance(excluded, list)
        or len(include) + len(excluded) > MAXIMUM_POLICY_PATTERNS
        or not all(isinstance(value, str) and value for value in include + excluded)
        or any(
            len(value.encode("utf-8")) > MAXIMUM_PATTERN_BYTES
            for value in include + excluded
        )
        or any(
            value != unicodedata.normalize("NFKC", value)
            or any(
                unicodedata.category(character).startswith("C")
                or is_default_ignorable(character)
                for character in value
            )
            for value in include + excluded
        )
    ):
        raise InventoryError("release-corpus policy has an unsupported shape")

    selected_objects: dict[str, str] = {}
    file_records: list[dict[str, Any]] = []
    selected_total_bytes = 0
    portable_path_authority: dict[tuple[str, ...], tuple[str, str]] = {}
    current_entries = commit_entries(root, commit)
    for path, mode, kind, object_id in current_entries:
        if not selected(path, include) or selected(path, excluded):
            continue
        register_portable_path(portable_path_authority, path)
        if kind != "blob" or mode not in REGULAR_MODES:
            raise InventoryError(f"selected path is not an ordinary Git file: {path}")
        if len(file_records) >= MAXIMUM_SELECTED_FILES:
            raise InventoryError("release corpus exceeds the selected-file bound")
        try:
            object_size = int(git(root, "cat-file", "-s", object_id).decode("ascii"))
        except ValueError as error:
            raise InventoryError(f"Git blob has an invalid size: {path}") from error
        if object_size > MAXIMUM_SELECTED_FILE_BYTES:
            raise InventoryError(f"selected file exceeds the size bound: {path}")
        selected_total_bytes += object_size
        if selected_total_bytes > MAXIMUM_SELECTED_TOTAL_BYTES:
            raise InventoryError("release corpus exceeds the total-byte bound")
        data = git(
            root,
            "cat-file",
            "blob",
            object_id,
            maximum_output_bytes=object_size,
        )
        if len(data) != object_size:
            raise InventoryError(f"Git blob size changed while reading: {path}")
        selected_objects[path] = object_id
        file_records.append(
            {
                "executable": mode == "100755",
                "path": path,
                "sha256": hashlib.sha256(data).hexdigest(),
                "size": len(data),
            }
        )
    if not file_records:
        raise InventoryError("release corpus selected no files")
    require_clean_repository(root)
    replayed_commit = (
        git(root, "rev-parse", "--verify", "HEAD^{commit}").decode("ascii").strip()
    )
    if replayed_commit != commit:
        raise InventoryError("HEAD changed while the inventory was being captured")

    inventory = {
        "corpusPolicySha256": hashlib.sha256(corpus_bytes).hexdigest(),
        "excludedCurrentEntryCount": len(current_entries) - len(file_records),
        "files": file_records,
        "purpose": PURPOSE,
        "schema": SCHEMA,
        "selectedFileCount": len(file_records),
        "selectedTotalBytes": selected_total_bytes,
        "sourceCommit": commit,
        "sourceTree": tree,
        "trackedEntryCount": len(current_entries),
    }
    return inventory, selected_objects


def canonical_zip_member(record: dict[str, Any]) -> zipfile.ZipInfo:
    member = zipfile.ZipInfo(record["path"], (1980, 1, 1, 0, 0, 0))
    member.create_system = 3
    permissions = 0o755 if record["executable"] else 0o644
    member.external_attr = (stat.S_IFREG | permissions) << 16
    member.compress_type = zipfile.ZIP_STORED
    return member


def write_archive(
    root: Path,
    output: Path,
    objects: dict[str, str],
    inventory: dict[str, Any],
) -> None:
    if output.exists() or output.is_symlink():
        raise InventoryError("output archive must not already exist")
    if not output.parent.is_dir() or output.parent.is_symlink():
        raise InventoryError("output archive parent must be an existing real directory")
    flags = os.O_RDWR | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(output, flags, 0o600)
    records = {record["path"]: record for record in inventory["files"]}
    try:
        with os.fdopen(descriptor, "w+b") as handle:
            with zipfile.ZipFile(
                handle,
                mode="w",
                compression=zipfile.ZIP_STORED,
                allowZip64=False,
            ) as archive:
                for relative, object_id in objects.items():
                    record = records[relative]
                    data = git(
                        root,
                        "cat-file",
                        "blob",
                        object_id,
                        maximum_output_bytes=record["size"],
                    )
                    if (
                        len(data) != record["size"]
                        or hashlib.sha256(data).hexdigest() != record["sha256"]
                    ):
                        raise InventoryError(
                            f"Git blob changed while archiving: {relative}"
                        )
                    archive.writestr(canonical_zip_member(record), data)
            handle.flush()
            os.fsync(handle.fileno())
            opened = os.fstat(handle.fileno())
            if not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1:
                raise InventoryError("output archive is not an ordinary file")
            handle.seek(0)
            verify_archive(handle, inventory)
    except (zipfile.BadZipFile, zipfile.LargeZipFile) as error:
        raise InventoryError(f"output archive is invalid: {error}") from error


def verify_archive(handle: BinaryIO, inventory: dict[str, Any]) -> None:
    expected = inventory["files"]
    observed_total_bytes = 0
    try:
        with tempfile.SpooledTemporaryFile(max_size=8 * 1024 * 1024) as canonical:
            with zipfile.ZipFile(handle, mode="r") as archive, zipfile.ZipFile(
                canonical,
                mode="w",
                compression=zipfile.ZIP_STORED,
                allowZip64=False,
            ) as replay:
                if archive.comment:
                    raise InventoryError("output archive has an unexpected comment")
                members = archive.infolist()
                if len(members) != len(expected):
                    raise InventoryError("output archive member count does not match")
                for member, record in zip(members, expected, strict=True):
                    safe_relative_path(member.filename)
                    permissions = 0o755 if record["executable"] else 0o644
                    expected_mode = stat.S_IFREG | permissions
                    try:
                        member.filename.encode("ascii")
                        expected_flags = 0
                    except UnicodeEncodeError:
                        expected_flags = 0x800
                    if (
                        member.filename != record["path"]
                        or member.is_dir()
                        or member.date_time != (1980, 1, 1, 0, 0, 0)
                        or member.create_system != 3
                        or member.create_version != 20
                        or member.extract_version != 20
                        or member.flag_bits != expected_flags
                        or member.internal_attr != 0
                        or member.volume != 0
                        or member.compress_type != zipfile.ZIP_STORED
                        or member.comment
                        or member.extra
                        or member.file_size != record["size"]
                        or member.compress_size != record["size"]
                        or member.external_attr >> 16 != expected_mode
                    ):
                        raise InventoryError(
                            "output archive member authority mismatch: "
                            f"{member.filename}"
                        )
                    data = archive.read(member)
                    observed_total_bytes += len(data)
                    if observed_total_bytes > MAXIMUM_SELECTED_TOTAL_BYTES:
                        raise InventoryError("output archive exceeds the total-byte bound")
                    if (
                        len(data) != record["size"]
                        or hashlib.sha256(data).hexdigest() != record["sha256"]
                    ):
                        raise InventoryError(
                            f"output archive member bytes mismatch: {member.filename}"
                        )
                    replay.writestr(canonical_zip_member(record), data)
            handle.seek(0)
            canonical.seek(0)
            while True:
                observed_chunk = handle.read(64 * 1024)
                canonical_chunk = canonical.read(64 * 1024)
                if observed_chunk != canonical_chunk:
                    raise InventoryError("output archive has non-canonical bytes")
                if not observed_chunk:
                    break
    except (zipfile.BadZipFile, zipfile.LargeZipFile) as error:
        raise InventoryError(f"output archive is invalid: {error}") from error


def write_manifest(path: Path, data: bytes, label: str = "manifest") -> None:
    if path.exists() or path.is_symlink():
        raise InventoryError(f"{label} destination must not already exist")
    if not path.parent.is_dir() or path.parent.is_symlink():
        raise InventoryError(f"{label} parent must be an existing real directory")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags, 0o600)
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())
        opened = os.fstat(handle.fileno())
        if not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1:
            raise InventoryError(f"{label} was not created as an ordinary file")


def destinations_overlap(first: Path, second: Path) -> bool:
    return os.path.samefile(first.parent, second.parent) and unicodedata.normalize(
        "NFKC", first.name
    ).casefold() == unicodedata.normalize("NFKC", second.name).casefold()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--repository-root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--output-archive", type=Path)
    parser.add_argument("--dependency-sbom", type=Path)
    arguments = parser.parse_args()

    try:
        root = arguments.repository_root.resolve(strict=True)
        inventory, files = build_inventory(root)
        manifest = canonical_json(inventory)
        dependency_sbom = None
        if arguments.dependency_sbom is not None:
            dependency_sbom = canonical_json(build_dependency_sbom(root, inventory))
        archive_path = None
        if arguments.output_archive is not None:
            archive_path = require_external_destination(
                arguments.output_archive, root, "output archive"
            )
        manifest_path = None
        if arguments.manifest is not None:
            manifest_path = require_external_destination(
                arguments.manifest, root, "manifest"
            )
        dependency_sbom_path = None
        if arguments.dependency_sbom is not None:
            dependency_sbom_path = require_external_destination(
                arguments.dependency_sbom, root, "dependency SBOM"
            )
        destinations = [
            ("output archive", archive_path),
            ("manifest", manifest_path),
            ("dependency SBOM", dependency_sbom_path),
        ]
        for first_index, (first_label, first_path) in enumerate(destinations):
            if first_path is None:
                continue
            for second_label, second_path in destinations[first_index + 1 :]:
                if second_path is not None and destinations_overlap(
                    first_path, second_path
                ):
                    raise InventoryError(
                        f"{first_label} and {second_label} must be different paths"
                    )
        if archive_path is not None:
            write_archive(root, archive_path, files, inventory)
        if manifest_path is not None:
            write_manifest(manifest_path, manifest)
        else:
            sys.stdout.buffer.write(manifest)
        if dependency_sbom_path is not None:
            assert dependency_sbom is not None
            write_manifest(dependency_sbom_path, dependency_sbom, "dependency SBOM")
    except (InventoryError, OSError) as error:
        print(f"PUBLIC_RELEASE_INVENTORY_INVALID: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
