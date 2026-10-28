#!/usr/bin/env python3
"""Bounded regression tests for the release-corpus vocabulary gate.

This test imports the gate directly and supplies a temporary corpus.  It does
not need a repository, network access, or a running service.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import stat
import sys
import tempfile
import warnings
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
GATE_PATH = ROOT / "scripts" / "release-corpus-vocabulary-gate.py"
SPEC = importlib.util.spec_from_file_location("release_corpus_gate", GATE_PATH)
assert SPEC and SPEC.loader
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


def write_zip(path: Path, entries: list[tuple[str, bytes]]) -> None:
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in entries:
            info = zipfile.ZipInfo(name)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            archive.writestr(info, data)


def expect_rejected(path: Path, label: str) -> None:
    try:
        gate.archive_texts(path)
    except ValueError:
        return
    raise AssertionError(f"{label} was accepted")


def mark_zip_encrypted(path: Path) -> None:
    """Set the general-purpose encryption flag without attempting extraction."""
    data = bytearray(path.read_bytes())
    for signature, flag_offset in ((b"PK\x03\x04", 6), (b"PK\x01\x02", 8)):
        start = 0
        while (index := data.find(signature, start)) >= 0:
            position = index + flag_offset
            flags = int.from_bytes(data[position : position + 2], "little") | 0x1
            data[position : position + 2] = flags.to_bytes(2, "little")
            start = index + len(signature)
    path.write_bytes(data)


def invoke(root: Path, paths: list[str]) -> tuple[int, str]:
    original_paths, original_argv = gate.tracked_paths, sys.argv
    output = io.StringIO()
    try:
        gate.tracked_paths = lambda _root: paths
        sys.argv = [str(GATE_PATH), "--repository-root", str(root)]
        with contextlib.redirect_stderr(output), contextlib.redirect_stdout(output):
            try:
                result = gate.main()
            except SystemExit as error:
                result = error.code if isinstance(error.code, int) else 1
    finally:
        gate.tracked_paths, sys.argv = original_paths, original_argv
    return result, output.getvalue()


def main() -> int:
    for filename in (".gitattributes", ".gitignore", "CODEOWNERS", "LICENSE", "NOTICE"):
        assert gate.text_name_allowed(filename)
    assert not gate.text_name_allowed("DEPENDENCY-NOTICES")
    swift_paths = [
        "native/macos/DocumentOpenAdapter.swift",
        "tests/native/macos/DocumentOpenAdapterTests.swift",
        "native/macos/ApplicationBootstrap.swift",
        "tests/native/macos/ApplicationBootstrapTests.swift",
    ]
    for name in swift_paths:
        assert gate.text_name_allowed(name), name
    for name in (
        "native/macos/Unreviewed.swift", "tests/native/macos/Extra.swift",
        "private/DocumentOpenAdapter.swift", "native/macos/DocumentOpenAdapter.SWIFT",
        "native/macos/ApplicationBootstrapExtra.swift",
        "tests/native/macos/ApplicationBootstrapExtraTests.swift",
        "native/macos/ApplicationBootstrap.SWIFT",
    ):
        assert not gate.text_name_allowed(name), name

    assert gate.text_name_allowed("web/src/favicon.svg")
    for name in ("web/src/unreviewed.svg", "assets/favicon.svg", "favicon.svg", "web/src/favicon.SVG"):
        assert not gate.text_name_allowed(name), name

    with tempfile.TemporaryDirectory(prefix="release-corpus-gate-") as temporary:
        root = Path(temporary)
        (root / "config").mkdir()
        (root / "fixtures").mkdir()
        (root / "config" / "release-corpus.json").write_text(
            json.dumps({"include": ["fixtures/**", "web/**", *swift_paths], "excluded": []}), encoding="utf-8"
        )

        asset = root / "web/src/favicon.svg"
        asset.parent.mkdir(parents=True)
        asset.write_text('<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1v1z"/></svg>')
        result, output = invoke(root, ["web/src/favicon.svg"])
        assert result == 0, output
        unreviewed = asset.with_name("unreviewed.svg")
        unreviewed.write_bytes(asset.read_bytes())
        result, output = invoke(root, ["web/src/unreviewed.svg"])
        assert result == 1 and "selected file has no approved text name" in output, output

        for name in swift_paths:
            swift = root / name
            swift.parent.mkdir(parents=True, exist_ok=True)
            swift.write_text('let title = "Synthetic document"\n', encoding="utf-8")
            result, output = invoke(root, [name])
            assert result == 0, output
            for data, reason in ((b"\xff", "not UTF-8"), (b"let x = 1\x00", "forbidden Unicode controls"), (b"PK\x03\x04payload", "archive bytes")):
                swift.write_bytes(data)
                result, output = invoke(root, [name])
                assert result == 1 and reason in output, output
            swift.write_text("let value = 1\n", encoding="utf-8")

        valid = root / "fixtures" / "customer-accounts.zip"
        write_zip(valid, [("customer_accounts.csv", b"account_id,name\n1,Ada\n")])
        assert gate.archive_texts(valid) == [
            ("customer_accounts.csv", "account_id,name\n1,Ada\n")
        ]
        result, output = invoke(root, ["fixtures/customer-accounts.zip"])
        assert result == 0, output

        retired_file = "business_" + "partners.csv"
        retired_entity = "Business" + "Partner"
        for name, content, reason in (
            (retired_file, b"account_id\n1\n", "legacy member name"),
            ("customer_accounts.csv", (retired_entity + "\n1\n").encode(), "legacy member content"),
        ):
            path = root / "fixtures" / f"{reason.replace(' ', '-')}.zip"
            write_zip(path, [(name, content)])
            result, output = invoke(root, [f"fixtures/{path.name}"])
            assert result == 1 and "gate rejected selected material" in output, output

        bidi_name = "customer" + chr(0x202E) + "accounts.csv"
        bidi = root / "fixtures" / "bidi.zip"
        write_zip(bidi, [(bidi_name, b"account_id\n1\n")])
        expect_rejected(bidi, "bidi member name")
        zero_width = root / "fixtures" / "zero-width.zip"
        write_zip(
            zero_width,
            [("customer_accounts.csv", ("S" + chr(0x200B) + "AP\n").encode())],
        )
        expect_rejected(zero_width, "zero-width member content")
        grapheme_joiner = root / "fixtures" / "grapheme-joiner.zip"
        write_zip(
            grapheme_joiner,
            [("customer_accounts.csv", ("S" + chr(0x034F) + "AP\n").encode())],
        )
        expect_rejected(grapheme_joiner, "default-ignorable grapheme joiner")
        variation_selector = root / "fixtures" / "variation-selector.zip"
        write_zip(
            variation_selector,
            [("customer_accounts.csv", ("S" + chr(0xFE0F) + "AP\n").encode())],
        )
        expect_rejected(variation_selector, "default-ignorable variation selector")
        combining_mark = root / "fixtures" / "combining-mark.zip"
        write_zip(
            combining_mark,
            [("customer_accounts.csv", ("S" + chr(0x0301) + "AP\n").encode())],
        )
        result, output = invoke(root, ["fixtures/combining-mark.zip"])
        assert result == 1 and "gate rejected selected material" in output, output
        full_width = root / "fixtures" / "full-width.zip"
        write_zip(full_width, [("customer_accounts.csv", "\uff33\uff21\uff30\n".encode())])
        result, output = invoke(root, ["fixtures/full-width.zip"])
        assert result == 1 and "gate rejected selected material" in output, output
        unicode_readme = root / "fixtures" / "unicode-readme.zip"
        write_zip(
            unicode_readme,
            [
                ("README.md", b"ordinary text\n"),
                ("\uff32\uff25\uff21\uff24\uff2d\uff25.md", b"ordinary text\n"),
            ],
        )
        expect_rejected(unicode_readme, "NFKC README member ambiguity")

        cases = {
            "traversal": [("../customer_accounts.csv", b"ok")],
            "absolute": [("/customer_accounts.csv", b"ok")],
            "backslash": [(r"dir\\customer_accounts.csv", b"ok")],
            "drive": [("C:customer_accounts.csv", b"ok")],
            "double-slash": [("dir//customer_accounts.csv", b"ok")],
            "dot-segment": [("dir/./customer_accounts.csv", b"ok")],
            "control": [("customer_accounts\x07.csv", b"ok")],
            "duplicate-normalized": [
                ("customer_accounts.csv", b"one"),
                ("CUSTOMER_ACCOUNTS.csv", b"two"),
            ],
            "nested": [("nested.zip", b"not an archive")],
            "unsupported-member": [("payload.bin", b"ok")],
            "non-utf8": [("customer_accounts.csv", b"\xff")],
        }
        for label, entries in cases.items():
            path = root / "fixtures" / f"{label}.zip"
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                write_zip(path, entries)
            expect_rejected(path, label)

        encrypted = root / "fixtures" / "encrypted.zip"
        write_zip(encrypted, [("customer_accounts.csv", b"ok")])
        mark_zip_encrypted(encrypted)
        expect_rejected(encrypted, "encrypted member")

        archive_comment = root / "fixtures" / "archive-comment.zip"
        write_zip(archive_comment, [("customer_accounts.csv", b"ok")])
        with zipfile.ZipFile(archive_comment, "a") as archive:
            archive.comment = b"unexpected"
        expect_rejected(archive_comment, "archive comment")

        member_comment = root / "fixtures" / "member-comment.zip"
        with zipfile.ZipFile(member_comment, "w") as archive:
            info = zipfile.ZipInfo("customer_accounts.csv")
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            info.comment = b"unexpected"
            archive.writestr(info, b"ok")
        expect_rejected(member_comment, "member comment")

        member_extra = root / "fixtures" / "member-extra.zip"
        with zipfile.ZipFile(member_extra, "w") as archive:
            info = zipfile.ZipInfo("customer_accounts.csv")
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            info.extra = b"\x01\x00\x00\x00"
            archive.writestr(info, b"ok")
        expect_rejected(member_extra, "member extra")

        inner = root / "fixtures" / "inner.zip"
        write_zip(inner, [("customer_accounts.csv", b"ok")])
        disguised_nested = root / "fixtures" / "disguised-nested.zip"
        write_zip(disguised_nested, [("nested.txt", inner.read_bytes())])
        expect_rejected(disguised_nested, "archive bytes disguised as text")

        nonregular = root / "fixtures" / "nonregular-member.zip"
        with zipfile.ZipFile(nonregular, "w") as archive:
            info = zipfile.ZipInfo("customer_accounts.csv")
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            archive.writestr(info, b"target.csv")
        expect_rejected(nonregular, "nonregular ZIP member")

        unsupported_compression = root / "fixtures" / "unsupported-compression.zip"
        with zipfile.ZipFile(
            unsupported_compression, "w", compression=zipfile.ZIP_BZIP2
        ) as archive:
            archive.writestr("customer_accounts.csv", b"ok")
        expect_rejected(unsupported_compression, "unsupported compression")

        # Exercise header/member, member-count, total-size, and ratio bounds
        # with tiny temporary limits rather than creating large test artifacts.
        original_limits = (
            gate.MAXIMUM_ARCHIVE_INPUT_BYTES,
            gate.MAXIMUM_ARCHIVE_MEMBER_BYTES,
            gate.MAXIMUM_ARCHIVE_MEMBERS,
            gate.MAXIMUM_ARCHIVE_TOTAL_BYTES,
            gate.MAXIMUM_ARCHIVE_COMPRESSION_RATIO,
        )
        try:
            gate.MAXIMUM_ARCHIVE_INPUT_BYTES = 1
            expect_rejected(valid, "archive input bound")
            gate.MAXIMUM_ARCHIVE_INPUT_BYTES = 1024 * 1024
            gate.MAXIMUM_ARCHIVE_MEMBER_BYTES = 4
            header = root / "fixtures" / "header-bound.zip"
            write_zip(header, [("customer_accounts.csv", b"12345")])
            expect_rejected(header, "member header bound")
            gate.MAXIMUM_ARCHIVE_MEMBER_BYTES = 1024
            gate.MAXIMUM_ARCHIVE_MEMBERS = 1
            count = root / "fixtures" / "count-bound.zip"
            write_zip(count, [("one.csv", b"1"), ("two.csv", b"2")])
            expect_rejected(count, "member count bound")
            gate.MAXIMUM_ARCHIVE_TOTAL_BYTES = 4
            total = root / "fixtures" / "total-bound.zip"
            write_zip(total, [("customer_accounts.csv", b"12345")])
            expect_rejected(total, "expanded total bound")
            gate.MAXIMUM_ARCHIVE_TOTAL_BYTES = 1024
            gate.MAXIMUM_ARCHIVE_COMPRESSION_RATIO = 2
            ratio = root / "fixtures" / "ratio-bound.zip"
            write_zip(ratio, [("customer_accounts.csv", b"0" * 256)])
            expect_rejected(ratio, "compression ratio bound")
        finally:
            (
                gate.MAXIMUM_ARCHIVE_INPUT_BYTES,
                gate.MAXIMUM_ARCHIVE_MEMBER_BYTES,
                gate.MAXIMUM_ARCHIVE_MEMBERS,
                gate.MAXIMUM_ARCHIVE_TOTAL_BYTES,
                gate.MAXIMUM_ARCHIVE_COMPRESSION_RATIO,
            ) = original_limits

        for filename, data, label in (
            ("standards.txt", b"RFC 3339, RFC 8785, EDMX, and OData.\n", "standards"),
            ("unknown.bin", b"ordinary bytes", "unknown suffix"),
            ("invalid.txt", b"\xff", "non-UTF8 text"),
        ):
            path = root / "fixtures" / filename
            path.write_bytes(data)
            result, output = invoke(root, [f"fixtures/{filename}"])
            if label == "standards":
                assert result == 0, output
            else:
                assert result == 1, output

        target = root / "fixtures" / "target.txt"
        target.write_text("ordinary Unicode data: M\u00fcnchen\n", encoding="utf-8")
        result, output = invoke(root, ["fixtures/target.txt"])
        assert result == 0, output
        link = root / "fixtures" / "linked.txt"
        link.symlink_to(target.name)
        result, output = invoke(root, ["fixtures/target.txt", "fixtures/linked.txt"])
        assert result == 1 and "ordinary regular file" in output, output

        # CPython does not expose FIFO creation on Windows. Keep the exact FIFO
        # proof everywhere it is available and use a directory on Windows so every
        # platform still exercises this selected non-regular-path rejection.
        special = root / "fixtures" / "special.txt"
        if hasattr(os, "mkfifo"):
            os.mkfifo(special)
        else:
            assert os.name == "nt", "FIFO construction unavailable outside Windows"
            special.mkdir()
        result, output = invoke(root, ["fixtures/target.txt", "fixtures/special.txt"])
        assert result == 1 and "ordinary regular file" in output, output

        hard_link = root / "fixtures" / "hard-linked.txt"
        os.link(target, hard_link)
        result, output = invoke(root, ["fixtures/standards.txt", "fixtures/hard-linked.txt"])
        assert result == 1 and "ordinary regular file" in output, output

    print("release corpus vocabulary gate self-test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
