#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repository_root="$(cd "${script_directory}/.." && pwd -P)"
git_executable="/usr/bin/git"
output_directory=""
allow_uncommitted_snapshot=false
create_zip_archive=true
build_root=""
requested_target=""
temporary_base=""

usage() {
  printf '%s\n' \
    "Usage: scripts/build-assisted-runtime-bundle.sh --output /absolute/path [--target target-triple] [--allow-uncommitted-snapshot] [--no-archive]" \
    "" \
    "Builds one native assisted local runtime bundle. The default requires a clean Git source."
}

fail() {
  printf 'Assisted runtime bundle build failed: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [[ -n "$build_root" && -n "$temporary_base" && "$build_root" == "$temporary_base"/migration-stream-bundle-build.* && -d "$build_root" && ! -L "$build_root" ]]; then
    rm -rf -- "$build_root"
  fi
}
trap cleanup EXIT INT TERM

while (($# > 0)); do
  case "$1" in
    --output)
      (($# >= 2)) || fail "--output requires an absolute directory path"
      output_directory="$2"
      shift 2
      ;;
    --allow-uncommitted-snapshot)
      allow_uncommitted_snapshot=true
      shift
      ;;
    --no-archive)
      create_zip_archive=false
      shift
      ;;
    --target)
      (($# >= 2)) || fail "--target requires one exact target triple"
      requested_target="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[[ -n "$output_directory" ]] || fail "--output is required"
[[ "$output_directory" == /* ]] || fail "--output must be an absolute path"
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) host_target="aarch64-apple-darwin" ;;
  Linux/x86_64) host_target="x86_64-unknown-linux-gnu" ;;
  Linux/aarch64) host_target="aarch64-unknown-linux-gnu" ;;
  *) fail "this host is not one native portable-bundle matrix target" ;;
esac
if [[ -n "$requested_target" && "$requested_target" != "$host_target" ]]; then
  fail "POSIX bundle assembly is native-only; requested $requested_target on $host_target"
fi
case "$host_target" in
  aarch64-apple-darwin) canonical_artifact_name="migration-stream-local-macos-arm64" ;;
  x86_64-unknown-linux-gnu) canonical_artifact_name="migration-stream-local-linux-x64-gnu" ;;
  aarch64-unknown-linux-gnu) canonical_artifact_name="migration-stream-local-linux-arm64-gnu" ;;
esac
[[ "$(basename "$output_directory")" == "$canonical_artifact_name" ]] ||
  fail "--output basename must be the canonical artifact name: $canonical_artifact_name"

[[ -f "$git_executable" && -x "$git_executable" && ! -L "$git_executable" ]] ||
  fail "the fixed Git executable must be one real executable file: $git_executable"
for command in python3 npm cargo rustc; do
  command -v "$command" >/dev/null 2>&1 || fail "missing build-time prerequisite: $command"
done
for variable in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS MACOSX_DEPLOYMENT_TARGET; do
  [[ -z "${!variable:-}" ]] ||
    fail "build-affecting environment variable must be unset: $variable"
done
if env | awk -F= '$1 ~ /^VITE_/ { found = 1 } END { exit found ? 0 : 1 }'; then
  fail "VITE_* environment variables are not accepted by the bounded bundle build"
fi

output_parent="$(dirname "$output_directory")"
[[ -d "$output_parent" && ! -L "$output_parent" ]] ||
  fail "the output parent must be one existing real directory"
output_directory="$(cd "$output_parent" && pwd -P)/$(basename "$output_directory")"
case "$output_directory" in
  "$repository_root"|"$repository_root"/*)
    fail "bundle output must remain outside the review repository"
    ;;
esac
archive_path="$(python3 - "$output_directory" <<'PY'
import pathlib
import sys
print(pathlib.Path(sys.argv[1]).with_suffix(".zip"))
PY
)"
archive_checksum_path="${archive_path}.sha256"
[[ ! -e "$output_directory" && ! -e "$archive_path" && ! -e "$archive_checksum_path" ]] ||
  fail "bundle output, archive or archive checksum already exists"

cd "$repository_root"
[[ -z "$("$git_executable" ls-files --unmerged)" ]] || fail "unmerged source paths are not supported"
[[ -z "$("$git_executable" ls-files --stage | awk '$1 == "160000" { print $4 }')" ]] ||
  fail "submodules are not supported by this bundle format"
"$git_executable" diff --cached --quiet || fail "staged changes are ambiguous; use one clean source"

temporary_base="${TMPDIR:-/tmp}"
[[ "$temporary_base" == /* && -d "$temporary_base" ]] ||
  fail "TMPDIR must name one existing absolute directory"
temporary_base="$(cd "$temporary_base" && pwd -P)"
[[ -d "$temporary_base" && ! -L "$temporary_base" ]] ||
  fail "TMPDIR must resolve to one existing real directory"

status_file="$(mktemp "$temporary_base/migration-stream-bundle-status.XXXXXX")"
"$git_executable" status --porcelain=v1 -z --untracked-files=all >"$status_file"
if [[ -s "$status_file" && "$allow_uncommitted_snapshot" != true ]]; then
  rm -f -- "$status_file"
  fail "source is dirty; review it and use --allow-uncommitted-snapshot only for an explicit local proof"
fi

product_commit="$("$git_executable" rev-parse --verify HEAD)"
[[ "$product_commit" =~ ^[0-9a-f]{40}$ ]] || fail "HEAD is not one full lowercase Git object ID"
source_state="clean"
if [[ -s "$status_file" ]]; then
  source_state="explicit-uncommitted-snapshot"
fi
rm -f -- "$status_file"

build_root="$(mktemp -d "$temporary_base/migration-stream-bundle-build.XXXXXX")"
chmod 700 "$build_root"
file_list="${build_root}/source-files.nul"
source_manifest="${build_root}/source-manifest.json"
frontend_assets_root="${build_root}/frontend-assets"
frontend_manifest="${build_root}/frontend-manifest.json"
release_binaries_root="${build_root}/release-binaries"

write_source_manifest() {
  local next_file_list="${file_list}.next"
  python3 scripts/release-corpus-vocabulary-gate.py \
    --repository-root "$repository_root" >&2
  "$git_executable" ls-files --cached --others --exclude-standard -z >"$next_file_list"
  mv -f -- "$next_file_list" "$file_list"
  python3 - "$repository_root" "$file_list" "$source_manifest" <<'PY'
import hashlib
import json
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1]).resolve()
entries = pathlib.Path(sys.argv[2]).read_bytes().split(b"\0")
if entries and entries[-1] == b"":
    entries.pop()
if not entries:
    raise SystemExit("Git-visible source inventory is empty")

records = []
seen = set()
for raw in entries:
    relative = raw.decode("utf-8", errors="strict")
    parts = pathlib.PurePosixPath(relative).parts
    if (
        not relative
        or relative in seen
        or pathlib.PurePosixPath(relative).is_absolute()
        or any(part in ("", ".", "..") for part in parts)
        or any(ord(character) < 32 or ord(character) == 127 for character in relative)
    ):
        raise SystemExit(f"unsafe or duplicate source path: {relative!r}")
    seen.add(relative)
    path = root
    missing = False
    for part in parts:
        path = path / part
        try:
            info = path.lstat()
        except FileNotFoundError:
            missing = True
            break
        if stat.S_ISLNK(info.st_mode):
            raise SystemExit(f"source path contains a symlink: {relative!r}")
    if missing:
        # A reviewed dirty snapshot may deliberately delete a tracked path.
        # The surrounding status and repeated snapshot checks bind that state.
        continue
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise SystemExit(f"source path is not a regular file: {relative!r}")
    data = path.read_bytes()
    records.append(
        {
            "path": relative,
            "executable": bool(stat.S_IMODE(info.st_mode) & 0o111),
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    )

records.sort(key=lambda item: item["path"].encode("utf-8"))
document = {
    "schemaVersion": "migration-stream/source-snapshot-manifest/v1",
    "files": records,
}
canonical = json.dumps(
    document, ensure_ascii=False, sort_keys=True, separators=(",", ":")
).encode("utf-8")
pathlib.Path(sys.argv[3]).write_bytes(canonical)
print(hashlib.sha256(canonical).hexdigest())
PY
}

verify_source_authority() {
  local observed_commit observed_state replay_status
  observed_commit="$("$git_executable" rev-parse --verify HEAD)"
  [[ "$observed_commit" == "$product_commit" ]] ||
    fail "repository HEAD changed during the bundle build"
  "$git_executable" diff --cached --quiet || fail "staged source changed during the bundle build"
  [[ -z "$("$git_executable" ls-files --unmerged)" ]] ||
    fail "unmerged source appeared during the bundle build"
  replay_status="${build_root}/source-status-replay.nul"
  "$git_executable" status --porcelain=v1 -z --untracked-files=all >"$replay_status"
  observed_state="clean"
  if [[ -s "$replay_status" ]]; then
    observed_state="explicit-uncommitted-snapshot"
  fi
  [[ "$observed_state" == "$source_state" ]] ||
    fail "repository clean/dirty classification changed during the bundle build"
}

write_frontend_manifest() {
  python3 - "$frontend_assets_root" "$frontend_manifest" <<'PY'
import hashlib
import json
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1]).resolve()
if not root.is_dir() or root.is_symlink():
    raise SystemExit("frontend staging root is not one real directory")

records = []
entries = 0
total = 0
for path in root.rglob("*"):
    entries += 1
    if entries > 66048:
        raise SystemExit("frontend asset graph exceeds its entry bound")
    relative_path = path.relative_to(root)
    parts = relative_path.parts
    if len(("web/" + relative_path.as_posix()).encode("utf-8")) > 512 or any(
        not part
        or not part[0].isascii()
        or not part[0].isalnum()
        or len(part.encode("utf-8")) > 512
        or any(not (character.isascii() and (character.isalnum() or character in "._-")) for character in part)
        for part in parts
    ):
        raise SystemExit(f"unsafe frontend asset path: {relative_path!s}")
    info = path.lstat()
    if stat.S_ISLNK(info.st_mode):
        raise SystemExit(f"frontend asset graph contains a link: {relative_path!s}")
    if stat.S_ISDIR(info.st_mode):
        continue
    if not stat.S_ISREG(info.st_mode):
        raise SystemExit(f"unsupported frontend asset type: {relative_path!s}")
    if stat.S_IMODE(info.st_mode) & 0o111:
        raise SystemExit(f"frontend asset is executable: {relative_path!s}")
    if len(records) >= 256 or info.st_size > 8 * 1024 * 1024:
        raise SystemExit("frontend asset file bounds were exceeded")
    total += info.st_size
    if total > 32 * 1024 * 1024:
        raise SystemExit("frontend asset bytes exceed their bound")
    data = path.read_bytes()
    if len(data) != info.st_size:
        raise SystemExit(f"frontend asset changed while reading: {relative_path!s}")
    records.append(
        {
            "path": relative_path.as_posix(),
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    )

if not records:
    raise SystemExit("frontend asset staging root is empty")
records.sort(key=lambda item: item["path"].encode("utf-8"))
document = {
    "schemaVersion": "migration-stream/frontend-asset-snapshot/v1",
    "files": records,
}

canonical = json.dumps(
    document, ensure_ascii=False, sort_keys=True, separators=(",", ":")
).encode("utf-8")
pathlib.Path(sys.argv[2]).write_bytes(canonical)
print(hashlib.sha256(canonical).hexdigest())
PY
}

# Cargo is permitted to deduplicate release outputs with hard links. Bundle
# members are deliberately not: their verifier rejects aliases so a later
# external write cannot silently alter a packaged member. Copy every executable
# used by assembly into this private build root without following links.
stage_release_binaries() {
  python3 - "$repository_root/target/release" "$release_binaries_root" <<'PY'
import hashlib
import os
import pathlib
import stat
import sys

source_root = pathlib.Path(sys.argv[1])
destination_root = pathlib.Path(sys.argv[2])
maximum_member_bytes = 128 * 1024 * 1024
maximum_total_bytes = 512 * 1024 * 1024
if not source_root.is_absolute() or not destination_root.is_absolute():
    raise SystemExit("release binary roots must be absolute")
for directory in (source_root.parent, source_root):
    directory_stat = directory.lstat()
    if not stat.S_ISDIR(directory_stat.st_mode) or stat.S_ISLNK(directory_stat.st_mode):
        raise SystemExit("Cargo release root is not one real directory")
source_root = source_root.resolve(strict=True)
if not source_root.is_dir():
    raise SystemExit("Cargo release root is not one real directory")
if destination_root.exists() or destination_root.is_symlink():
    raise SystemExit("private release binary staging root already exists")
destination_parent = destination_root.parent.lstat()
if not stat.S_ISDIR(destination_parent.st_mode) or stat.S_ISLNK(destination_parent.st_mode):
    raise SystemExit("private release binary staging parent is not one real directory")
destination_root.mkdir(mode=0o700)
destination_root_stat = destination_root.lstat()
if not stat.S_ISDIR(destination_root_stat.st_mode) or stat.S_IMODE(destination_root_stat.st_mode) != 0o700:
    raise SystemExit("private release binary staging root has unsafe mode")

def same_identity(left, right):
    return left.st_dev == right.st_dev and left.st_ino == right.st_ino

total_bytes = 0
for name in (
    "migration-stream-local-runtime",
    "migration-stream",
    "migration-stream-workbench",
    "migration-stream-bundle",
):
    source = source_root / name
    before = source.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode):
        raise SystemExit(f"Cargo output is not one real regular file: {name}")
    if before.st_size > maximum_member_bytes:
        raise SystemExit(f"Cargo output exceeds the staged member bound: {name}")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    source_descriptor = os.open(source, flags)
    try:
        opened = os.fstat(source_descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or not same_identity(before, opened)
            or opened.st_size != before.st_size
            or opened.st_size > maximum_member_bytes
            or total_bytes + opened.st_size > maximum_total_bytes
        ):
            raise SystemExit(f"Cargo output changed while opening: {name}")
        destination = destination_root / name
        destination_descriptor = os.open(
            destination,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
            0o500,
        )
        try:
            os.fchmod(destination_descriptor, 0o500)
            digest = hashlib.sha256()
            observed = 0
            while True:
                chunk = os.read(source_descriptor, 65536)
                if not chunk:
                    break
                observed += len(chunk)
                if observed > maximum_member_bytes or total_bytes + observed > maximum_total_bytes:
                    raise SystemExit(f"Cargo output exceeds the staged byte bounds: {name}")
                digest.update(chunk)
                offset = 0
                while offset < len(chunk):
                    written = os.write(destination_descriptor, chunk[offset:])
                    if written <= 0:
                        raise SystemExit(f"staged release binary write failed: {name}")
                    offset += written
            os.fsync(destination_descriptor)
            copied = os.fstat(destination_descriptor)
            if (
                not stat.S_ISREG(copied.st_mode)
                or copied.st_nlink != 1
                or stat.S_IMODE(copied.st_mode) != 0o500
                or copied.st_size != observed
                or observed != opened.st_size
            ):
                raise SystemExit(f"staged release binary is not single-link: {name}")
        finally:
            os.close(destination_descriptor)
        after = os.fstat(source_descriptor)
        named = source.lstat()
        if (
            not same_identity(opened, after)
            or not same_identity(after, named)
            or opened.st_size != after.st_size
            or opened.st_mtime_ns != after.st_mtime_ns
            or not stat.S_ISREG(named.st_mode)
            or stat.S_ISLNK(named.st_mode)
        ):
            raise SystemExit(f"Cargo output changed while staging: {name}")
        staged = destination.lstat()
        if not stat.S_ISREG(staged.st_mode) or stat.S_ISLNK(staged.st_mode) or staged.st_nlink != 1:
            raise SystemExit(f"staged release binary identity is unsafe: {name}")
        if staged.st_size != observed:
            raise SystemExit(f"staged release binary length changed: {name}")
        total_bytes += observed
        print(f"{name} {digest.hexdigest()}")
    finally:
        os.close(source_descriptor)
directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0)
directory_descriptor = os.open(destination_root, directory_flags)
try:
    os.fsync(directory_descriptor)
finally:
    os.close(directory_descriptor)
PY
}

source_snapshot_sha256="$(write_source_manifest)"
[[ "$source_snapshot_sha256" =~ ^[0-9a-f]{64}$ ]] ||
  fail "source snapshot hash is invalid"
verify_source_authority

npm --prefix web ci --offline --no-audit --no-fund
npm --prefix web run typecheck
mkdir "$frontend_assets_root"
(
  cd web
  ./node_modules/.bin/vite build --outDir "$frontend_assets_root" --emptyOutDir
)
frontend_assets_sha256="$(write_frontend_manifest)"
[[ "$frontend_assets_sha256" =~ ^[0-9a-f]{64}$ ]] ||
  fail "frontend asset snapshot hash is invalid"
MIGRATION_STREAM_PRODUCT_COMMIT="$product_commit" \
MIGRATION_STREAM_SOURCE_SNAPSHOT_SHA256="$source_snapshot_sha256" \
MIGRATION_STREAM_SOURCE_STATE="$source_state" \
MIGRATION_STREAM_FRONTEND_ASSETS_SHA256="$frontend_assets_sha256" \
  bash "$repository_root/scripts/cargo-build.sh" build --locked --release --bins

stage_release_binaries

if [[ "$host_target" == "aarch64-apple-darwin" ]]; then
python3 - \
  "$release_binaries_root/migration-stream-local-runtime" \
  "$release_binaries_root/migration-stream" \
  "$release_binaries_root/migration-stream-workbench" <<'PY'
import pathlib
import re
import subprocess
import sys

for raw_path in sys.argv[1:]:
    path = pathlib.Path(raw_path).resolve()
    file_result = subprocess.run(
        ["/usr/bin/file", str(path)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    if "Mach-O 64-bit executable arm64" not in file_result or "universal" in file_result.lower():
        raise SystemExit(f"release executable is not one thin arm64 Mach-O: {path.name}")

    load_commands = subprocess.run(
        ["/usr/bin/otool", "-l", str(path)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    minimum_versions = re.findall(r"^\s*minos\s+([0-9.]+)\s*$", load_commands, re.MULTILINE)
    if minimum_versions != ["11.0"]:
        raise SystemExit(
            f"release executable has unexpected macOS minimum version {minimum_versions}: {path.name}"
        )

    linked = subprocess.run(
        ["/usr/bin/otool", "-L", str(path)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()[1:]
    dependencies = [
        line.strip().split(" (compatibility version", 1)[0]
        for line in linked
        if line.strip()
    ]
    if not dependencies or any(
        not dependency.startswith(("/usr/lib/", "/System/Library/"))
        for dependency in dependencies
    ):
        raise SystemExit(
            f"release executable has a non-system or absent dependency graph: {path.name}"
        )
PY
fi

source_snapshot_after="$(write_source_manifest)"
[[ "$source_snapshot_after" == "$source_snapshot_sha256" ]] ||
  fail "source bytes changed during the build"
verify_source_authority
frontend_assets_after="$(write_frontend_manifest)"
[[ "$frontend_assets_after" == "$frontend_assets_sha256" ]] ||
  fail "frontend asset bytes changed during the build"

assembler=("$release_binaries_root/migration-stream-bundle"
  --repository-root "$repository_root"
  --git-executable "$git_executable"
  --frontend-assets-root "$frontend_assets_root"
  --release-binaries-root "$release_binaries_root"
  --output-directory "$output_directory"
  --product-commit "$product_commit"
  --source-state "$source_state"
  --source-snapshot-sha256 "$source_snapshot_sha256"
  --frontend-assets-sha256 "$frontend_assets_sha256")
if [[ "$create_zip_archive" == true ]]; then
  assembler+=(--create-zip-archive)
fi
"${assembler[@]}"
