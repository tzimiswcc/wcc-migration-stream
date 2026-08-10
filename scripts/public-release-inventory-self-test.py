#!/usr/bin/env python3
"""Self-test the deterministic, non-publishing public-release inventory."""

from __future__ import annotations

import importlib.util
import base64
import hashlib
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path


SCRIPT = Path(__file__).with_name("public-release-inventory.py").resolve()


def run(*arguments: str, cwd: Path | None = None) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, "-B", str(SCRIPT), *arguments],
        cwd=cwd,
        check=False,
        capture_output=True,
    )


def git(root: Path, *arguments: str) -> None:
    subprocess.run(["git", *arguments], cwd=root, check=True, capture_output=True)


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def clone_fixture(source: Path, destination: Path) -> None:
    subprocess.run(
        ["git", "clone", "--quiet", str(source), str(destination)],
        check=True,
        capture_output=True,
    )
    git(destination, "config", "user.name", "Inventory Test")
    git(destination, "config", "user.email", "inventory@example.invalid")


def load_module():
    specification = importlib.util.spec_from_file_location(
        "public_release_inventory", SCRIPT
    )
    if specification is None or specification.loader is None:
        raise AssertionError("inventory module could not be loaded")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="public-release-inventory-test-") as temporary:
        base = Path(temporary)
        module = load_module()
        real_repository = base / "real-repository"
        clone_fixture(SCRIPT.parents[1], real_repository)
        real_inventory, _ = module.build_inventory(real_repository)
        real_sbom = module.build_dependency_sbom(real_repository, real_inventory)
        replayed_real_sbom = module.build_dependency_sbom(
            real_repository, real_inventory
        )
        assert module.canonical_json(real_sbom) == module.canonical_json(
            replayed_real_sbom
        )
        real_properties = {
            property_["name"]: property_["value"]
            for property_ in real_sbom["metadata"]["properties"]
        }
        assert real_properties["migration-stream:evidence:component-count"] == "503"
        assert real_properties[
            "migration-stream:evidence:cargo-component-count"
        ] == "231"
        assert real_properties[
            "migration-stream:evidence:npm-component-count"
        ] == "272"
        assert real_properties[
            "migration-stream:evidence:direct-declaration-count"
        ] == "51"
        assert real_properties[
            "migration-stream:evidence:direct-component-count"
        ] == "49"
        assert len(real_sbom["components"]) == 503
        assert "dependencies" not in real_sbom
        assert all("licenses" not in component for component in real_sbom["components"])
        assert all(
            component["name"] != "1.97.1" for component in real_sbom["components"]
        )

        repository = base / "repository"
        repository.mkdir()
        git(repository, "init", "--quiet")
        git(repository, "config", "user.name", "Inventory Test")
        git(repository, "config", "user.email", "inventory@example.invalid")

        write(
            repository / "config/release-corpus.json",
            json.dumps(
                {
                    "schemaVersion": 1,
                    "purpose": "test",
                    "include": [
                        "Cargo.lock",
                        "Cargo.toml",
                        "config/release-corpus.json",
                        "crates/**",
                        "public/**",
                        "rust-toolchain.toml",
                        "web/**",
                    ],
                    "excluded": ["public/private/**"],
                },
                sort_keys=True,
            ),
        )
        write(repository / "public/readme.txt", "public bytes\n")
        executable = repository / "public/run.sh"
        write(executable, "#!/bin/sh\nexit 0\n")
        executable.chmod(0o755)
        write(repository / "public/private/secret.txt", "excluded\n")
        write(repository / "internal.txt", "internal\n")
        write(
            repository / "Cargo.toml",
            """[package]
name = "fixture"
version = "1.0.0"
rust-version = "1.97.1"

[workspace]
members = ["crates/helper"]

[dependencies]
helper = { path = "crates/helper" }
serde = "=1.0.0"
""",
        )
        write(
            repository / "crates/helper/Cargo.toml",
            """[package]
name = "helper"
version = "1.0.0"
rust-version = "1.97.1"

[dependencies]
anyhow = "=1.0.0"
""",
        )
        write(
            repository / "Cargo.lock",
            """version = 4

[[package]]
name = "fixture"
version = "1.0.0"
dependencies = ["helper", "serde"]

[[package]]
name = "helper"
version = "1.0.0"
dependencies = ["anyhow"]

[[package]]
name = "anyhow"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
""",
        )
        write(
            repository / "rust-toolchain.toml",
            """[toolchain]
channel = "1.97.1"
profile = "minimal"
""",
        )
        write(
            repository / "web/package.json",
            json.dumps(
                {
                    "name": "fixture-web",
                    "version": "0.0.0",
                    "private": True,
                    "dependencies": {"left-pad": "1.0.0"},
                    "devDependencies": {"fixture-tool": "2.0.0"},
                    "optionalDependencies": {"fixture-tool": "2.0.0"},
                    "engines": {"node": "^20.19.0 || >=24.0.0"},
                },
                sort_keys=True,
            ),
        )
        integrity_a = "sha512-" + base64.b64encode(bytes([17]) * 64).decode()
        integrity_b = "sha512-" + base64.b64encode(bytes([34]) * 64).decode()
        write(
            repository / "web/package-lock.json",
            json.dumps(
                {
                    "name": "fixture-web",
                    "version": "0.0.0",
                    "lockfileVersion": 3,
                    "requires": True,
                    "packages": {
                        "": {
                            "name": "fixture-web",
                            "version": "0.0.0",
                            "dependencies": {"left-pad": "1.0.0"},
                            "devDependencies": {"fixture-tool": "2.0.0"},
                            "optionalDependencies": {"fixture-tool": "2.0.0"},
                            "engines": {"node": "^20.19.0 || >=24.0.0"},
                        },
                        "node_modules/left-pad": {
                            "version": "1.0.0",
                            "resolved": "https://registry.npmjs.org/left-pad/-/left-pad-1.0.0.tgz",
                            "integrity": integrity_a,
                            "license": "MIT OR Apache-2.0",
                            "dependencies": {"fixture-tool": "2.0.0"},
                            "peerDependencies": {
                                "fixture-tool": "2.0.0",
                                "missing-peer": "1.0.0",
                            },
                            "peerDependenciesMeta": {
                                "missing-peer": {"optional": True}
                            },
                        },
                        "node_modules/fixture-tool": {
                            "version": "2.0.0",
                            "resolved": (
                                "https://registry.npmjs.org/fixture-tool/"
                                "-/fixture-tool-2.0.0.tgz"
                            ),
                            "integrity": integrity_b,
                            "license": "UNREVIEWED-FIXTURE",
                            "dev": True,
                        },
                    },
                },
                sort_keys=True,
            ),
        )
        write(repository / "web/.node-version", "24.18.0\n")
        git(repository, "add", ".")
        git(repository, "update-index", "--chmod=+x", "public/run.sh")
        git(repository, "commit", "--quiet", "-m", "fixture")

        first = base / "first.json"
        second = base / "second.json"
        result = run("--repository-root", str(repository), "--manifest", str(first))
        assert result.returncode == 0, result.stderr.decode()
        result = run("--repository-root", str(repository), "--manifest", str(second))
        assert result.returncode == 0, result.stderr.decode()
        assert first.read_bytes() == second.read_bytes()

        document = json.loads(first.read_text(encoding="utf-8"))
        assert document["schema"] == "migration-stream/public-release-inventory/v1"
        assert document["selectedFileCount"] == 10
        assert [record["path"] for record in document["files"]] == [
            "Cargo.lock",
            "Cargo.toml",
            "config/release-corpus.json",
            "crates/helper/Cargo.toml",
            "public/readme.txt",
            "public/run.sh",
            "rust-toolchain.toml",
            "web/.node-version",
            "web/package-lock.json",
            "web/package.json",
        ]
        assert next(
            record for record in document["files"] if record["path"] == "public/run.sh"
        )["executable"] is True

        first_sbom = base / "first.cdx.json"
        second_sbom = base / "second.cdx.json"
        result = run(
            "--repository-root",
            str(repository),
            "--dependency-sbom",
            str(first_sbom),
        )
        assert result.returncode == 0, result.stderr.decode()
        result = run(
            "--repository-root",
            str(repository),
            "--dependency-sbom",
            str(second_sbom),
        )
        assert result.returncode == 0, result.stderr.decode()
        assert first_sbom.read_bytes() == second_sbom.read_bytes()
        sbom = json.loads(first_sbom.read_text(encoding="utf-8"))
        assert sbom["$schema"] == "https://cyclonedx.org/schema/bom-1.6.schema.json"
        assert sbom["bomFormat"] == "CycloneDX"
        assert sbom["specVersion"] == "1.6"
        assert sbom["version"] == 1
        assert "serialNumber" not in sbom
        assert "timestamp" not in sbom["metadata"]
        assert len(sbom["components"]) == 4
        assert "dependencies" not in sbom
        assert len({component["bom-ref"] for component in sbom["components"]}) == 4
        assert all(
            component["bom-ref"].startswith("urn:migration-stream:dependency:")
            for component in sbom["components"]
        )
        assert all("licenses" not in component for component in sbom["components"])
        left_pad = next(
            component for component in sbom["components"] if component["name"] == "left-pad"
        )
        assert {
            property_["name"]: property_["value"]
            for property_ in left_pad["properties"]
        }[
            "migration-stream:evidence:unreviewed-npm-lockfile-license-string"
        ] == "MIT OR Apache-2.0"
        assert left_pad["hashes"] == [
            {"alg": "SHA-512", "content": (bytes([17]) * 64).hex()}
        ]
        sbom_properties = {
            property_["name"]: property_["value"]
            for property_ in sbom["metadata"]["properties"]
        }
        assert sbom_properties["migration-stream:evidence:component-count"] == "4"
        assert sbom_properties[
            "migration-stream:evidence:direct-declaration-count"
        ] == "5"
        assert sbom_properties[
            "migration-stream:evidence:direct-component-count"
        ] == "4"
        assert sbom_properties["migration-stream:evidence:node-pin"] == "24.18.0"
        assert sbom_properties[
            "migration-stream:evidence:node-engine"
        ] == "^20.19.0 || >=24.0.0"
        assert sbom_properties[
            "migration-stream:evidence:corpus-policy-sha256"
        ] == document["corpusPolicySha256"]
        assert sbom_properties[
            "migration-stream:evidence:inventory-sha256"
        ] == hashlib.sha256(module.canonical_json(document)).hexdigest()
        assert int(
            sbom_properties["migration-stream:evidence:authority-total-bytes"]
        ) > 0

        for unsafe in (
            "public/CON.txt",
            "public/alternate:stream.txt",
            "public/trailing.",
            "public/hidden\u200bname.txt",
        ):
            try:
                module.safe_relative_path(unsafe)
            except module.InventoryError:
                pass
            else:
                raise AssertionError(f"non-portable path was accepted: {unsafe!r}")
        for unsafe_npm_name in (".", "@scope/..", "Uppercase"):
            try:
                module.npm_identity(unsafe_npm_name, "test npm identity")
            except module.InventoryError:
                pass
            else:
                raise AssertionError(
                    f"noncanonical npm identity was accepted: {unsafe_npm_name!r}"
                )
        assert module.portable_path_key("public/Readme.txt") == module.portable_path_key(
            "PUBLIC/readme.TXT"
        )
        authority: dict[tuple[str, ...], tuple[str, str]] = {}
        module.register_portable_path(authority, "public/Foo/a.txt")
        try:
            module.register_portable_path(authority, "public/foo/b.txt")
        except module.InventoryError:
            pass
        else:
            raise AssertionError("case-colliding directory spellings were accepted")
        authority = {}
        module.register_portable_path(authority, "public/Foo/a.txt")
        try:
            module.register_portable_path(authority, "public/foo")
        except module.InventoryError:
            pass
        else:
            raise AssertionError("file/directory prefix collision was accepted")

        original_policy_bound = module.MAXIMUM_POLICY_BYTES
        module.MAXIMUM_POLICY_BYTES = 4
        try:
            module.build_inventory(repository)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("oversized policy was accepted")
        finally:
            module.MAXIMUM_POLICY_BYTES = original_policy_bound

        original_file_bound = module.MAXIMUM_SELECTED_FILE_BYTES
        module.MAXIMUM_SELECTED_FILE_BYTES = 4
        try:
            module.build_inventory(repository)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("oversized selected file was accepted")
        finally:
            module.MAXIMUM_SELECTED_FILE_BYTES = original_file_bound

        original_dependency_bound = module.MAXIMUM_DEPENDENCY_INPUT_BYTES
        module.MAXIMUM_DEPENDENCY_INPUT_BYTES = 4
        try:
            module.build_dependency_sbom(repository, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("oversized dependency authority was accepted")
        finally:
            module.MAXIMUM_DEPENDENCY_INPUT_BYTES = original_dependency_bound

        original_authority_bound = module.MAXIMUM_DEPENDENCY_AUTHORITY_TOTAL_BYTES
        fixed_authority_paths = (
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            "web/package.json",
            "web/package-lock.json",
            "web/.node-version",
        )
        fixed_authority_bytes = sum(
            (repository / path).stat().st_size for path in fixed_authority_paths
        )
        member_bytes = (repository / "crates/helper/Cargo.toml").stat().st_size
        module.MAXIMUM_DEPENDENCY_AUTHORITY_TOTAL_BYTES = (
            fixed_authority_bytes + member_bytes - 1
        )
        try:
            module.build_dependency_sbom(repository, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("oversized aggregate dependency authority was accepted")
        finally:
            module.MAXIMUM_DEPENDENCY_AUTHORITY_TOTAL_BYTES = original_authority_bound

        original_sbom_bound = module.MAXIMUM_DEPENDENCY_SBOM_BYTES
        module.MAXIMUM_DEPENDENCY_SBOM_BYTES = 4
        try:
            module.build_dependency_sbom(repository, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("oversized canonical dependency SBOM was accepted")
        finally:
            module.MAXIMUM_DEPENDENCY_SBOM_BYTES = original_sbom_bound

        forged_inventory = dict(document)
        forged_inventory["corpusPolicySha256"] = "0" * 64
        try:
            module.build_dependency_sbom(repository, forged_inventory)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("forged inventory corpus binding was accepted")

        original_bom_ref = module.authority_bom_ref
        module.authority_bom_ref = lambda *_values: "urn:duplicate"
        try:
            module.build_dependency_sbom(repository, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("colliding dependency BOM references were accepted")
        finally:
            module.authority_bom_ref = original_bom_ref

        original_cargo_purl = module.cargo_purl
        module.cargo_purl = lambda *_values: "pkg:cargo/duplicate@1.0.0"
        try:
            module.build_dependency_sbom(repository, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("colliding package URLs were accepted")
        finally:
            module.cargo_purl = original_cargo_purl

        archive = base / "public-release-candidate.zip"
        third = base / "third.json"
        result = run(
            "--repository-root",
            str(repository),
            "--manifest",
            str(third),
            "--output-archive",
            str(archive),
        )
        assert result.returncode == 0, result.stderr.decode()
        assert third.read_bytes() == first.read_bytes()
        with zipfile.ZipFile(archive) as candidate:
            assert candidate.namelist() == [
                record["path"] for record in document["files"]
            ]
            assert candidate.read("public/readme.txt") == b"public bytes\n"

        replay_archive = base / "public-release-candidate-replay.zip"
        replay = run(
            "--repository-root",
            str(repository),
            "--output-archive",
            str(replay_archive),
        )
        assert replay.returncode == 0, replay.stderr.decode()
        assert replay_archive.read_bytes() == archive.read_bytes()
        with replay_archive.open("ab") as handle:
            handle.write(b"UNVERIFIED_TRAILER")
        try:
            with replay_archive.open("r+b") as handle:
                module.verify_archive(handle, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("archive trailing bytes were accepted")

        wrong_metadata = io.BytesIO()
        with zipfile.ZipFile(
            wrong_metadata, mode="w", compression=zipfile.ZIP_STORED
        ) as candidate:
            with zipfile.ZipFile(archive) as source:
                for index, record in enumerate(document["files"]):
                    timestamp = (
                        (1981, 1, 1, 0, 0, 0)
                        if index == 0
                        else (1980, 1, 1, 0, 0, 0)
                    )
                    member = zipfile.ZipInfo(record["path"], timestamp)
                    member.create_system = 3
                    permissions = 0o755 if record["executable"] else 0o644
                    member.external_attr = (stat.S_IFREG | permissions) << 16
                    member.compress_type = zipfile.ZIP_STORED
                    candidate.writestr(member, source.read(record["path"]))
        wrong_metadata.seek(0)
        try:
            module.verify_archive(wrong_metadata, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("archive metadata tampering was accepted")

        with archive.open("r+b") as handle:
            module.verify_archive(handle, document)
        with zipfile.ZipFile(archive, mode="a", compression=zipfile.ZIP_STORED) as candidate:
            candidate.writestr("unexpected.txt", b"tampered")
        try:
            with archive.open("r+b") as handle:
                module.verify_archive(handle, document)
        except module.InventoryError:
            pass
        else:
            raise AssertionError("archive tampering was accepted")

        overlap = base / "overlap.zip"
        overlap_result = run(
            "--repository-root",
            str(repository),
            "--manifest",
            str(overlap),
            "--output-archive",
            str(overlap),
        )
        assert overlap_result.returncode != 0
        assert not overlap.exists()

        manifest_sbom_overlap = base / "manifest-sbom-overlap.json"
        overlap_result = run(
            "--repository-root",
            str(repository),
            "--manifest",
            str(manifest_sbom_overlap),
            "--dependency-sbom",
            str(manifest_sbom_overlap),
        )
        assert overlap_result.returncode != 0
        assert not manifest_sbom_overlap.exists()

        archive_sbom_overlap = base / "archive-sbom-overlap.bin"
        overlap_result = run(
            "--repository-root",
            str(repository),
            "--output-archive",
            str(archive_sbom_overlap),
            "--dependency-sbom",
            str(archive_sbom_overlap),
        )
        assert overlap_result.returncode != 0
        assert not archive_sbom_overlap.exists()

        casefold_manifest = base / "CASEFOLD-EVIDENCE.json"
        casefold_sbom = base / "casefold-evidence.json"
        overlap_result = run(
            "--repository-root",
            str(repository),
            "--manifest",
            str(casefold_manifest),
            "--dependency-sbom",
            str(casefold_sbom),
        )
        assert overlap_result.returncode != 0
        assert not casefold_manifest.exists()
        assert not casefold_sbom.exists()

        preexisting_sbom = base / "preexisting-sbom.json"
        write(preexisting_sbom, "reserved\n")
        unwritten_archive = base / "must-remain-unwritten.zip"
        preexisting_result = run(
            "--repository-root",
            str(repository),
            "--output-archive",
            str(unwritten_archive),
            "--dependency-sbom",
            str(preexisting_sbom),
        )
        assert preexisting_result.returncode != 0
        assert not unwritten_archive.exists()
        assert preexisting_sbom.read_text(encoding="utf-8") == "reserved\n"

        unsafe_output = run(
            "--repository-root",
            str(repository),
            "--dependency-sbom",
            str(base / "CON.json"),
        )
        assert unsafe_output.returncode != 0
        assert b"filename must be portable" in unsafe_output.stderr
        assert not (base / "CON.json").exists()

        existing_sbom = run(
            "--repository-root",
            str(repository),
            "--dependency-sbom",
            str(first_sbom),
        )
        assert existing_sbom.returncode != 0
        assert b"must not already exist" in existing_sbom.stderr

        inside = run(
            "--repository-root",
            str(repository),
            "--output-archive",
            str(repository / "export.zip"),
        )
        assert inside.returncode != 0
        assert b"must be outside the repository" in inside.stderr

        inside_sbom = run(
            "--repository-root",
            str(repository),
            "--dependency-sbom",
            str(repository / "dependencies.cdx.json"),
        )
        assert inside_sbom.returncode != 0
        assert b"must be outside the repository" in inside_sbom.stderr

        alternate_case_repository = repository.parent / repository.name.upper()
        if alternate_case_repository.exists() and os.path.samefile(
            alternate_case_repository, repository
        ):
            alternate_case = run(
                "--repository-root",
                str(repository),
                "--output-archive",
                str(alternate_case_repository / "case-escape.zip"),
            )
            assert alternate_case.returncode != 0
            assert b"must be outside the repository" in alternate_case.stderr

        broken = base / "broken.zip"
        try:
            broken.symlink_to(base / "missing-target")
        except OSError:
            pass
        else:
            broken_result = run(
                "--repository-root",
                str(repository),
                "--output-archive",
                str(broken),
            )
            assert broken_result.returncode != 0
            assert broken.is_symlink()

        def invalid_dependency_case(name, mutation, expected: bytes) -> None:
            candidate = base / f"invalid-{name}"
            clone_fixture(repository, candidate)
            mutation(candidate)
            git(candidate, "add", ".")
            git(candidate, "commit", "--quiet", "-m", f"invalid {name}")
            output = base / f"invalid-{name}.cdx.json"
            invalid = run(
                "--repository-root",
                str(candidate),
                "--dependency-sbom",
                str(output),
            )
            assert invalid.returncode != 0, name
            assert expected in invalid.stderr, (name, invalid.stderr.decode())
            assert not output.exists()

        def mutate_cargo_checksum(candidate: Path) -> None:
            path = candidate / "Cargo.lock"
            write(path, path.read_text(encoding="utf-8").replace("a" * 64, "bad", 1))

        invalid_dependency_case(
            "cargo-checksum", mutate_cargo_checksum, b"checksum is invalid"
        )

        def mutate_missing_cargo_checksum(candidate: Path) -> None:
            path = candidate / "Cargo.lock"
            write(
                path,
                path.read_text(encoding="utf-8").replace(
                    'checksum = "' + "a" * 64 + '"\n', "", 1
                ),
            )

        invalid_dependency_case(
            "cargo-missing-checksum",
            mutate_missing_cargo_checksum,
            b"checksum is invalid",
        )

        def mutate_cargo_pin(candidate: Path) -> None:
            path = candidate / "Cargo.toml"
            write(path, path.read_text(encoding="utf-8").replace('"=1.0.0"', '"1.0.0"'))

        invalid_dependency_case(
            "cargo-direct-pin", mutate_cargo_pin, b"must be an exact '=version' pin"
        )

        def mutate_cargo_source(candidate: Path) -> None:
            path = candidate / "Cargo.lock"
            write(
                path,
                path.read_text(encoding="utf-8").replace(
                    "registry+https://github.com/rust-lang/crates.io-index",
                    "git+https://example.invalid/dependency",
                    1,
                ),
            )

        invalid_dependency_case(
            "cargo-source", mutate_cargo_source, b"unsupported source"
        )

        def mutate_cargo_ambiguous(candidate: Path) -> None:
            path = candidate / "Cargo.lock"
            write(
                path,
                path.read_text(encoding="utf-8")
                + """
[[package]]
name = "serde"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
""",
            )

        invalid_dependency_case(
            "cargo-ambiguous", mutate_cargo_ambiguous, b"unresolved or ambiguous"
        )

        def mutate_local_external_purl_collision(candidate: Path) -> None:
            path = candidate / "Cargo.lock"
            write(
                path,
                path.read_text(encoding="utf-8")
                + """
[[package]]
name = "fixture"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
""",
            )

        invalid_dependency_case(
            "global-purl-collision",
            mutate_local_external_purl_collision,
            b"package URLs must be globally unique",
        )

        def mutate_extra_cargo_lock_edge(candidate: Path) -> None:
            path = candidate / "Cargo.lock"
            write(
                path,
                path.read_text(encoding="utf-8").replace(
                    'dependencies = ["helper", "serde"]',
                    'dependencies = ["anyhow", "helper", "serde"]',
                    1,
                ),
            )

        invalid_dependency_case(
            "cargo-extra-lock-edge",
            mutate_extra_cargo_lock_edge,
            b"direct dependencies drifted from Cargo.lock",
        )

        def mutate_toolchain(candidate: Path) -> None:
            path = candidate / "rust-toolchain.toml"
            write(path, path.read_text(encoding="utf-8").replace("1.97.1", "1.98.0"))

        invalid_dependency_case(
            "toolchain-drift", mutate_toolchain, b"does not match the toolchain"
        )

        def mutate_node_pin(candidate: Path) -> None:
            write(candidate / "web/.node-version", "19.0.0\n")

        invalid_dependency_case(
            "node-pin", mutate_node_pin, b"does not satisfy the Node engine"
        )

        def mutate_npm(candidate: Path, callback) -> None:
            manifest_path = candidate / "web/package.json"
            lock_path = candidate / "web/package-lock.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            lock = json.loads(lock_path.read_text(encoding="utf-8"))
            callback(manifest, lock)
            write(manifest_path, json.dumps(manifest, sort_keys=True))
            write(lock_path, json.dumps(lock, sort_keys=True))

        def mutate_npm_integrity(candidate: Path) -> None:
            mutate_npm(
                candidate,
                lambda _manifest, lock: lock["packages"][
                    "node_modules/left-pad"
                ].__setitem__("integrity", "sha512-not-base64"),
            )

        invalid_dependency_case(
            "npm-integrity", mutate_npm_integrity, b"integrity is malformed"
        )

        def mutate_missing_npm_integrity(candidate: Path) -> None:
            mutate_npm(
                candidate,
                lambda _manifest, lock: lock["packages"][
                    "node_modules/left-pad"
                ].pop("integrity"),
            )

        invalid_dependency_case(
            "npm-missing-integrity",
            mutate_missing_npm_integrity,
            b"integrity for node_modules/left-pad is invalid",
        )

        def mutate_npm_identity(candidate: Path) -> None:
            mutate_npm(
                candidate,
                lambda _manifest, lock: lock["packages"][
                    "node_modules/left-pad"
                ].pop("version"),
            )

        invalid_dependency_case(
            "npm-identity", mutate_npm_identity, b"does not match the lockfile"
        )

        def mutate_npm_pin(candidate: Path) -> None:
            def change(manifest, lock) -> None:
                manifest["dependencies"]["left-pad"] = "^1.0.0"
                lock["packages"][""]["dependencies"]["left-pad"] = "^1.0.0"

            mutate_npm(candidate, change)

        invalid_dependency_case(
            "npm-direct-pin", mutate_npm_pin, b"must be an exact version pin"
        )

        def mutate_npm_root(candidate: Path) -> None:
            mutate_npm(
                candidate,
                lambda _manifest, lock: lock["packages"][""].__setitem__(
                    "version", "9.9.9"
                ),
            )

        invalid_dependency_case(
            "npm-root", mutate_npm_root, b"root identity drifted"
        )

        def mutate_npm_link(candidate: Path) -> None:
            mutate_npm(
                candidate,
                lambda _manifest, lock: lock["packages"][
                    "node_modules/fixture-tool"
                ].__setitem__("link", True),
            )

        invalid_dependency_case("npm-link", mutate_npm_link, b"link package")

        def mutate_engine_root_drift(candidate: Path) -> None:
            mutate_npm(
                candidate,
                lambda manifest, _lock: manifest["engines"].__setitem__(
                    "node", ">=24.0.0"
                ),
            )

        invalid_dependency_case(
            "engine-root-drift", mutate_engine_root_drift, b"root engines drifted"
        )

        def mutate_incompatible_engine(candidate: Path) -> None:
            def change(manifest, lock) -> None:
                manifest["engines"]["node"] = "^22.0.0"
                lock["packages"][""]["engines"]["node"] = "^22.0.0"

            mutate_npm(candidate, change)

        invalid_dependency_case(
            "engine-incompatible",
            mutate_incompatible_engine,
            b"does not satisfy the Node engine",
        )

        def mutate_unsupported_engine(candidate: Path) -> None:
            def change(manifest, lock) -> None:
                manifest["engines"]["node"] = "~24.0.0"
                lock["packages"][""]["engines"]["node"] = "~24.0.0"

            mutate_npm(candidate, change)

        invalid_dependency_case(
            "engine-grammar",
            mutate_unsupported_engine,
            b"unsupported grammar",
        )

        def mutate_root_peer(candidate: Path) -> None:
            def change(manifest, lock) -> None:
                manifest["peerDependencies"] = {"left-pad": "1.0.0"}
                lock["packages"][""]["peerDependencies"] = {
                    "left-pad": "1.0.0"
                }

            mutate_npm(candidate, change)

        invalid_dependency_case(
            "root-peer", mutate_root_peer, b"root peerDependencies is unsupported"
        )

        def mutate_malformed_resolved_url(candidate: Path) -> None:
            mutate_npm(
                candidate,
                lambda _manifest, lock: lock["packages"][
                    "node_modules/left-pad"
                ].__setitem__("resolved", "https://[invalid"),
            )

        invalid_dependency_case(
            "npm-malformed-url",
            mutate_malformed_resolved_url,
            b"resolved URL is malformed",
        )

        def mutate_public_corpus(candidate: Path) -> None:
            path = candidate / "config/release-corpus.json"
            policy = json.loads(path.read_text(encoding="utf-8"))
            policy["include"].remove("Cargo.lock")
            write(path, json.dumps(policy, sort_keys=True))

        invalid_dependency_case(
            "public-corpus",
            mutate_public_corpus,
            b"outside the selected public corpus",
        )

        def mutate_duplicate_json_key(candidate: Path) -> None:
            path = candidate / "web/package.json"
            text = path.read_text(encoding="utf-8")
            write(
                path,
                text.replace(
                    '"name": "fixture-web"',
                    '"name": "fixture-web", "name": "duplicate"',
                    1,
                ),
            )

        invalid_dependency_case(
            "duplicate-json-key", mutate_duplicate_json_key, b"duplicate key"
        )

        write(repository / "dirty.txt", "dirty\n")
        dirty = run("--repository-root", str(repository))
        assert dirty.returncode != 0
        assert b"repository must be clean" in dirty.stderr

    print("public release inventory self-test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
