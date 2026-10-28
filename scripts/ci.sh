#!/usr/bin/env bash
set -euo pipefail

python3 scripts/release-corpus-vocabulary-gate.py
python3 scripts/release-corpus-vocabulary-gate-self-test.py
python3 -B scripts/public-release-inventory-self-test.py
python3 -B scripts/cargo-build-self-test.py
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
npm --prefix web ci --no-audit --no-fund
npm --prefix web run ci
