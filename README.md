# Migration Stream

Migration Stream is a local, evidence-first workbench for preparing and
checking structured migration data. It helps a practitioner define a bounded
destination contract, accept a ZIP/CSV source package, build and approve a
mapping contract, validate the result, and preserve the evidence needed to
understand what happened.

Migration Stream is published by World Class Consultants G.P.

It is designed for local, practitioner-led work. It does not connect to an
external target, write to one, deploy a service, or claim target compatibility.
The approved canonical profile and mapping—not imported metadata or browser
labels—are the authority used for validation.

## What is available now

- A startup-selected, durable local Object Workspace.
- Offline bounded metadata import or a manual destination definition, followed
  by review, freeze, and a separate exact-hash approval.
- Bounded ZIP/CSV source intake with sample-free structural profiling.
- Entity-neutral mapping with supported transformations, relationships, exact
  local reference tables, value-free coverage, freeze, and separate approval.
- Local validation, value-suppressed issue evidence, and governed remediation
  packs with explicitly selected keys and an optional acknowledged offending
  value.
- Immutable source revisions and adjacent CLOSED / REMAINING / NEW comparison.
- An eligible single-entity path through Decisions, Reconciliation, local
  Result Approval, and deterministic local Readiness Packages.

Multi-entity work currently stops at read-only First Issues and Compared
Validation. It does not enable downstream Decisions, Reconciliation, Result
Approval, or Package authority.

## Important boundaries

- Input is currently ZIP-of-CSV; the current output is a local Readiness ZIP.
- Mapping supports a deliberately bounded set of transformations and exact
  lookups. It does not execute arbitrary code, reshape rows, or fan out data.
- Offline metadata parsing preserves technical evidence; it is not an external
  target compatibility statement.
- Loopback binding reduces exposure but is not authentication.
- Package generation is local evidence creation. It does not transmit data or
  write to another system.
- The planned first public release is source-only `v0.1.0`. It will not include
  public binaries or designate any supported operating-system platform.
- Native platform evidence is development evidence. It is not a public binary,
  installer, signing, distribution, or platform-support promise.

## Start here

Choose the path that matches how you are evaluating the project:

- [Contributing and development](docs/CONTRIBUTING.md) — configure a source
  checkout, run the local checks, and follow the contribution terms.
- [Portable runtime matrix](docs/RUNTIME-TARGET-MATRIX.md) — exact artifact
  targets, accepted native evidence, and remaining release decisions.
- [Practitioner journey](docs/PRACTITIONER-JOURNEY.md) — what each stage does
  and where authority stops.
- [Authority and evidence](docs/AUTHORITY-AND-EVIDENCE.md) — why review,
  approval, hashes, and immutable history are separate.
- [Troubleshooting](docs/TROUBLESHOOTING.md) — safe local recovery steps.
- [Security and data boundary](SECURITY.md) — local-data and reporting limits.
- [Release and support status](docs/RELEASE-AND-SUPPORT.md) — current scope,
  release-candidate work, and no-support-promise boundary.
- [Route to a public release](docs/PUBLIC-RELEASE-ROUTE.md) — clean-export-first
  milestones and the owner decisions still required before publication.

The in-app **Help for this stage** control is local, read-only help for the ten
journey stages. It follows the current route and does not change authority.

## Source checks

The project pins its Rust and frontend dependencies. From a configured source
checkout:

```sh
bash scripts/ci.sh
cd web && npm ci --no-audit --no-fund && npm run format:check && npm run lint && npm run typecheck && npm test -- --run && npm run build
```

The public-export vocabulary gate is intentionally separate from product tests:

```sh
python3 scripts/release-corpus-vocabulary-gate.py
python3 scripts/release-corpus-vocabulary-gate-self-test.py
python3 -B scripts/public-release-inventory-self-test.py
```

After building `web/dist`, run a private local evaluation workspace from the
source checkout:

```sh
cargo run --locked --release --bin migration-stream-workbench -- \
  --workspace /absolute/private/workspace \
  --engagement-setup \
  --assets web/dist \
  --listen 127.0.0.1:4175
```

Use only synthetic or separately governed input. Loopback binding is not an
authentication boundary, and this source-build path is not a supported-platform
designation.

Read [the release corpus note](docs/RELEASE-CORPUS.md) before treating a
passing gate as publication readiness. A passing gate checks only its scoped
technical hygiene; it does not grant legal, contractual, security, or release
authority.

## Licence

Copyright 2026 World Class Consultants G.P.

Except where a file states otherwise, owner-authored material is licensed under
the [Apache License, Version 2.0](LICENSE). Third-party or adapted text retains
the terms and attribution stated in its file. See [NOTICE](NOTICE) for project
attribution and the Apache licence's trademark boundary.
