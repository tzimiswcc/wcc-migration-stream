#!/usr/bin/env python3
"""Validate and enumerate the deterministic clean-public-export corpus."""

from __future__ import annotations

import argparse
import fnmatch
import json
import stat
import subprocess
import sys
import unicodedata
import zipfile
from pathlib import Path, PurePosixPath


MAXIMUM_ARCHIVE_MEMBERS = 128
MAXIMUM_ARCHIVE_INPUT_BYTES = 16 * 1024 * 1024
MAXIMUM_ARCHIVE_MEMBER_BYTES = 16 * 1024 * 1024
MAXIMUM_ARCHIVE_TOTAL_BYTES = 64 * 1024 * 1024
MAXIMUM_ARCHIVE_COMPRESSION_RATIO = 100
TEXT_SUFFIXES = {
    ".css",
    ".csv",
    ".edmx",
    ".html",
    ".js",
    ".json",
    ".lock",
    ".md",
    ".mjs",
    ".ps1",
    ".py",
    ".rs",
    ".sh",
    ".toml",
    ".ts",
    ".tsx",
    ".txt",
    ".xml",
    ".yaml",
    ".yml",
}
# Only these reviewed Swift sources are classified as UTF-8; no generic suffix approval.
SWIFT_TEXT_PATHS = {
    "native/macos/DocumentOpenAdapter.swift",
    "tests/native/macos/DocumentOpenAdapterTests.swift",
    "native/macos/ApplicationBootstrap.swift",
    "tests/native/macos/ApplicationBootstrapTests.swift",
    "native/macos/MigrationStreamApplication.swift",
}
TEXT_FILENAMES = {
    ".gitattributes",
    ".gitignore",
    ".node-version",
    ".prettierignore",
    "CODEOWNERS",
    "Cargo.lock",
    "Cargo.toml",
    "LICENSE",
    "NOTICE",
    "README.md",
    "SECURITY.md",
}
NESTED_ARCHIVE_SUFFIXES = {
    ".7z",
    ".bz2",
    ".gz",
    ".rar",
    ".tar",
    ".xz",
    ".zip",
    ".zst",
}
PERMITTED_TEXT_CONTROLS = {"\t", "\n", "\r"}


def tracked_paths(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return sorted(
        (entry.decode("utf-8") for entry in result.stdout.split(b"\0") if entry),
        key=lambda value: value.encode("utf-8"),
    )


def selected(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def forbidden_patterns() -> list[str]:
    vendor = "s" + "ap"
    retired_account = "Business" + "Partner"
    retired_prefix = "b" + "p"
    retired_vendor_entity = "A_" + retired_account
    return [
        rf"\b{vendor}\b",
        r"\bs/4\b",
        "Synthetic_" + "S4",
        r"\bs/?4" + "ha" + "na" + r"\b",
        r"\b" + "ha" + "na" + r"\b",
        r"\b" + "ab" + "ap" + r"\b",
        r"\bremote function call\b",
        r"\br" + "fc" + r"[- ](?:destination|connection|adapter)\b",
        r"\b" + "ba" + "pi" + r"\b",
        r"\b" + "id" + "oc" + r"\b",
        "www." + vendor + ".com",
        vendor + ":",
        r"api[_-]business[_-]partner",
        r"canonical[_-]" + retired_prefix + r"[_-]readiness",
        r"\b" + retired_account + r"\w*",
        r"\bBusiness[ _-]?Partners?\b",
        r"\b" + retired_prefix + r"\b",
        r"business_" + r"partners\.csv",
        r"sanitized-" + retired_prefix + r"(?:\.|-|_)\w*",
        retired_vendor_entity + r"\w*",
        r"to_" + retired_account + r"\w*",
        r"to_" + "Customer" + r"\w*",
        retired_account + "Category",
        "Organization" + "BPName1",
        "Synthetic." + "SapStyle" + ".Sales",
        vendor.upper() + "Data",
    ]


def safe_member_name(name: str) -> bool:
    member = PurePosixPath(name)
    return (
        bool(name)
        and "\\" not in name
        and ":" not in name
        and not contains_forbidden_unicode(name, allow_text_whitespace=False)
        and not member.is_absolute()
        and not name.startswith("./")
        and "//" not in name
        and "/./" not in name
        and all(part not in ("", ".", "..") for part in member.parts)
    )


def contains_forbidden_unicode(value: str, *, allow_text_whitespace: bool) -> bool:
    for character in value:
        category = unicodedata.category(character)
        if category == "Cf" or is_default_ignorable(character):
            return True
        if category == "Cc" and (
            not allow_text_whitespace or character not in PERMITTED_TEXT_CONTROLS
        ):
            return True
    return False


def is_default_ignorable(character: str) -> bool:
    """Bounded Unicode default-ignorable set relevant to text identifiers."""
    codepoint = ord(character)
    return (
        codepoint in {0x034F, 0x061C, 0x115F, 0x1160, 0x17B4, 0x17B5, 0x3164, 0xFEFF, 0xFFA0, 0xE0000, 0xE0001}
        or 0x180B <= codepoint <= 0x180F
        or 0x200B <= codepoint <= 0x200F
        or 0x202A <= codepoint <= 0x202E
        or 0x2060 <= codepoint <= 0x206F
        or 0xFE00 <= codepoint <= 0xFE0F
        or 0x1BCA0 <= codepoint <= 0x1BCA3
        or 0x1D173 <= codepoint <= 0x1D17A
        or 0xE0020 <= codepoint <= 0xE007F
    )


def vocabulary_skeleton(value: str) -> str:
    """Expose vocabulary hidden by canonical decomposition or combining marks."""
    return "".join(
        character
        for character in unicodedata.normalize("NFKD", value)
        if not unicodedata.category(character).startswith("M")
        and not is_default_ignorable(character)
        and unicodedata.category(character) != "Cf"
    )


def archive_magic(data: bytes) -> bool:
    """Recognize archives by bytes, never merely by their file-name suffix."""
    signatures = (
        b"PK\x03\x04",  # ZIP
        b"PK\x05\x06",
        b"PK\x07\x08",
        b"\x1f\x8b",  # GZIP
        b"BZh",  # BZIP2
        b"\xfd7zXZ\x00",  # XZ
        b"7z\xbc\xaf\x27\x1c",
        b"Rar!\x1a\x07\x00",
        b"Rar!\x1a\x07\x01\x00",
        b"\x28\xb5\x2f\xfd",  # Zstandard
    )
    return data.startswith(signatures) or (len(data) >= 262 and data[257:262] == b"ustar")


def text_name_allowed(name: str) -> bool:
    path = PurePosixPath(name)
    # This reviewed local asset is UTF-8 source text; other SVG paths remain unapproved.
    return (
        name in SWIFT_TEXT_PATHS
        or name == "web/src/favicon.svg"
        or path.name in TEXT_FILENAMES
        or path.suffix.lower() in TEXT_SUFFIXES
    )


def archive_texts(path: Path) -> list[tuple[str, str]]:
    if path.stat().st_size > MAXIMUM_ARCHIVE_INPUT_BYTES:
        raise ValueError("ZIP input exceeds the allowed bound")
    try:
        archive = zipfile.ZipFile(path)
    except (OSError, zipfile.BadZipFile) as error:
        raise ValueError(f"invalid ZIP archive: {error}") from error
    with archive:
        if archive.comment:
            raise ValueError("ZIP archive comment is not allowed")
        members = archive.infolist()
        if not members or len(members) > MAXIMUM_ARCHIVE_MEMBERS:
            raise ValueError("ZIP member count is outside the allowed bounds")
        total = 0
        names: set[str] = set()
        texts: list[tuple[str, str]] = []
        for member in members:
            mode = member.external_attr >> 16
            nfkc_name = unicodedata.normalize("NFKC", member.filename)
            normalized_name = str(PurePosixPath(nfkc_name)).casefold()
            if (
                member.is_dir()
                or not safe_member_name(member.filename)
                or member.filename != nfkc_name
                or not text_name_allowed(member.filename)
                or normalized_name in names
                or member.flag_bits & 0x1
                or member.comment
                or member.extra
                or member.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)
                or member.file_size > MAXIMUM_ARCHIVE_MEMBER_BYTES
                or member.compress_size > MAXIMUM_ARCHIVE_MEMBER_BYTES
                or (
                    member.file_size
                    and member.file_size > member.compress_size * MAXIMUM_ARCHIVE_COMPRESSION_RATIO
                )
                or PurePosixPath(member.filename).suffix.lower() in NESTED_ARCHIVE_SUFFIXES
                or (mode and not stat.S_ISREG(mode))
            ):
                raise ValueError(f"unsafe ZIP member: {member.filename!r}")
            names.add(normalized_name)
            total += member.file_size
            if total > MAXIMUM_ARCHIVE_TOTAL_BYTES:
                raise ValueError("ZIP expanded size exceeds the allowed bound")
            data = archive.read(member)
            if len(data) != member.file_size:
                raise ValueError(f"ZIP member size mismatch: {member.filename!r}")
            if archive_magic(data):
                raise ValueError(f"nested archive bytes: {member.filename!r}")
            try:
                text = data.decode("utf-8")
            except UnicodeDecodeError as error:
                raise ValueError(f"ZIP member is not UTF-8: {member.filename!r}") from error
            if contains_forbidden_unicode(text, allow_text_whitespace=True):
                raise ValueError(f"ZIP member contains forbidden Unicode controls: {member.filename!r}")
            texts.append((member.filename, text))
        return texts


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository-root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--write-file-list", type=Path)
    args = parser.parse_args()
    root = args.repository_root.resolve()
    corpus = json.loads((root / "config/release-corpus.json").read_text(encoding="utf-8"))
    candidates = [
        path
        for path in tracked_paths(root)
        if selected(path, corpus["include"])
        and not selected(path, corpus["excluded"])
    ]
    paths: list[str] = []
    findings: list[str] = []
    for relative in candidates:
        path = root / relative
        try:
            info = path.lstat()
        except FileNotFoundError:
            # A dirty rename can leave an obsolete tracked entry behind. It is
            # not release material, while an existing special entry is unsafe.
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            findings.append(f"{relative}: selected path is not an ordinary regular file")
            continue
        paths.append(relative)
    if not paths:
        raise SystemExit("release corpus is empty")

    import re

    patterns = [re.compile(value, re.IGNORECASE) for value in forbidden_patterns()]

    def inspect_text(label: str, text: str) -> None:
        label = unicodedata.normalize("NFKC", label)
        text = unicodedata.normalize("NFKC", text)
        if contains_forbidden_unicode(label, allow_text_whitespace=False):
            findings.append(f"{label}: forbidden Unicode controls")
            return
        if contains_forbidden_unicode(text, allow_text_whitespace=True):
            findings.append(f"{label}: forbidden Unicode controls")
            return
        scan_label = vocabulary_skeleton(label)
        scan_text = vocabulary_skeleton(text)
        if any(pattern.search(scan_label) for pattern in patterns):
            findings.append(label)
        for line_number, line in enumerate(scan_text.splitlines(), start=1):
            if any(pattern.search(line) for pattern in patterns):
                findings.append(f"{label}:{line_number}")

    for relative in paths:
        path = root / relative
        if path.suffix.lower() == ".zip":
            try:
                for member_name, text in archive_texts(path):
                    inspect_text(f"{relative}!{member_name}", text)
            except (ValueError, OSError, RuntimeError, zipfile.BadZipFile) as error:
                findings.append(f"{relative}: {error}")
            continue
        if not text_name_allowed(relative):
            findings.append(f"{relative}: selected file has no approved text name")
            continue
        try:
            data = path.read_bytes()
            if archive_magic(data):
                findings.append(f"{relative}: archive bytes require an approved .zip container")
                continue
            inspect_text(relative, data.decode("utf-8"))
        except UnicodeDecodeError:
            findings.append(f"{relative}: selected non-ZIP file is not UTF-8")
    if findings:
        print("release corpus gate rejected selected material:", file=sys.stderr)
        print("\n".join(findings), file=sys.stderr)
        return 1
    if args.write_file_list:
        args.write_file_list.write_bytes(b"\0".join(path.encode("utf-8") for path in paths) + b"\0")
    print(f"release corpus vocabulary gate passed ({len(paths)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
