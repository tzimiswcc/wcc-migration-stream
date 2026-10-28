#!/usr/bin/env bash
# Rust 1.99.1 macOS ships rust-objcopy with an rpath that misses its bundled LLVM.
# Scope the workaround to compiler children; cargo-run applications inherit no new DYLD setting.
set -euo pipefail
script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
if [[ "$(uname -s)" == Darwin && "$(rustc --version)" == "rustc 1.99.1 "* ]]; then
  if [[ -n "${RUSTC_WRAPPER:-}" || -n "${RUSTC_WORKSPACE_WRAPPER:-}" ]]; then
    printf '%s\n' "Rust 1.99.1 macOS loader workaround cannot safely compose an existing RUSTC_WRAPPER or RUSTC_WORKSPACE_WRAPPER; leave that wrapper unchanged and configure its compiler-child loader environment explicitly." >&2
    exit 2
  fi
  export RUSTC_WRAPPER="${script_directory}/rustc-macos-loader.py"
fi
exec cargo "$@"
