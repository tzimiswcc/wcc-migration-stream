"""Check committed Markdown links without interpreting code as prose."""

from __future__ import annotations

import argparse
import re
import subprocess
import tempfile
from collections.abc import Iterable, Iterator
from pathlib import Path
from urllib.parse import unquote


LINK = re.compile(r"\[[^\]]+\]\(([^)]+)\)")
FENCE = re.compile(r"^[ \t]{0,3}(`{3,}|~{3,})")
EXTERNAL_PREFIXES = ("#", "https://", "http://", "mailto:")


def strip_inline_code(line: str) -> str:
    """Remove matched backtick code spans while preserving surrounding prose."""

    visible: list[str] = []
    index = 0
    while index < len(line):
        if line[index] != "`":
            visible.append(line[index])
            index += 1
            continue

        run_end = index
        while run_end < len(line) and line[run_end] == "`":
            run_end += 1
        marker = line[index:run_end]
        close = line.find(marker, run_end)
        if close < 0:
            visible.append(marker)
            index = run_end
            continue

        visible.append(" ")
        index = close + len(marker)

    return "".join(visible)


def rendered_prose(lines: Iterable[str]) -> Iterator[tuple[int, str]]:
    """Yield line numbers and prose outside Markdown code regions."""

    fence_character: str | None = None
    fence_length = 0
    for line_number, line in enumerate(lines, start=1):
        fence = FENCE.match(line)
        if fence is not None:
            marker = fence.group(1)
            if fence_character is None:
                fence_character = marker[0]
                fence_length = len(marker)
                continue
            if (
                marker[0] == fence_character
                and len(marker) >= fence_length
                and not line[fence.end() :].strip()
            ):
                fence_character = None
                fence_length = 0
            continue

        if fence_character is not None:
            continue
        if line.startswith(("    ", "\t")):
            continue
        yield line_number, strip_inline_code(line)


def check_markdown(paths: Iterable[Path]) -> list[str]:
    failures: list[str] = []
    for markdown in paths:
        lines = markdown.read_text(encoding="utf-8").splitlines()
        for line_number, line in rendered_prose(lines):
            for match in LINK.finditer(line):
                destination = match.group(1).strip().strip("<>")
                if not destination or destination.startswith(EXTERNAL_PREFIXES):
                    continue
                destination = unquote(destination.split("#", 1)[0])
                if not (markdown.parent / destination).exists():
                    failures.append(
                        f"{markdown}:{line_number}: missing {destination}"
                    )
    return failures


def committed_markdown_paths() -> list[Path]:
    names = subprocess.check_output(
        ["git", "ls-files", "--", "*.md"], text=True
    ).splitlines()
    return [Path(name) for name in names]


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="markdown-link-check-") as root:
        directory = Path(root)
        (directory / "present.md").write_text("present\n", encoding="utf-8")
        document = directory / "document.md"
        document.write_text(
            "\n".join(
                (
                    "[present](present.md)",
                    "[missing](missing.md)",
                    "`[inline code](missing-inline.md)`",
                    "    [indented code](missing-indented.md)",
                    "```powershell",
                    "[string](Get-Item -LiteralPath $fileLink -Force)",
                    "[fenced code](missing-fenced.md)",
                    "```",
                    "~~~text",
                    "[tilde fenced code](missing-tilde.md)",
                    "~~~",
                )
            ),
            encoding="utf-8",
        )
        failures = check_markdown((document,))
        expected = f"{document}:2: missing missing.md"
        if failures != [expected]:
            raise SystemExit(
                "Markdown link checker self-test failed: " + repr(failures)
            )
    print("Markdown link checker self-test passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    if arguments.self_test:
        self_test()
        return 0

    paths = committed_markdown_paths()
    failures = check_markdown(paths)
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"checked {len(paths)} committed Markdown documents")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
