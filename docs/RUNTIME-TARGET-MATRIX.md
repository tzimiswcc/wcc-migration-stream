# Portable runtime target matrix

Migration Stream packages one portable bundle per operating system and CPU
architecture. A bundle verifies only its own manifest, executable format and
host target. It is never a universal binary.

| Artifact name | Target | Retained pre-publication evidence | Release claim |
| --- | --- | --- | --- |
| `migration-stream-local-macos-arm64` | `aarch64-apple-darwin` | Native macOS arm64 bundle and browser lifecycle acceptance | Accepted local evaluation artifact; not an installer or signed release |
| `migration-stream-local-windows-x64` | `x86_64-pc-windows-msvc` | Native x64 owner-device proof and hosted compile, test, bundle and lifecycle acceptance passed | Accepted native evidence for a local-evaluation candidate; not publicly distributed, signed or covered by a support promise |
| `migration-stream-local-windows-arm64` | `aarch64-pc-windows-msvc` | Native ARM64 owner-device compile, test, bundle and lifecycle acceptance passed; CI cross-contract passed | Accepted native-device evidence for a local-evaluation candidate; not publicly distributed, signed or covered by a support promise |
| `migration-stream-local-linux-x64-gnu` | `x86_64-unknown-linux-gnu` | Native POSIX bundle assembly/verification lane | Not released pending full native browser/lifecycle acceptance |
| `migration-stream-local-linux-arm64-gnu` | `aarch64-unknown-linux-gnu` | Cross-build/static contract lane | Not supported or released pending native-device acceptance |

The canonical machine-readable matrix is
[`config/assisted-runtime-target-matrix.json`](../config/assisted-runtime-target-matrix.json).
Its status records evidence, not a promise of availability.

The machine-readable Windows statuses deliberately remain conservative until a
separately reviewed implementation change promotes them. This document records
completed evidence; it does not publish artifacts, create an installer, grant a
support commitment or authorize a public release.

## Pre-publication Windows evidence

Native Windows x64 and ARM64 validation was completed during private
release-candidate preparation. Private repository object IDs and hosted-run
URLs are intentionally not part of this clean source corpus. The retained
observations below provide engineering confidence only; they are not
independently reproducible public-release evidence and do not designate a
publicly supported artifact. Any future Windows binary release must bind fresh
native acceptance and published evidence to its exact public source commit and
artifact bytes.

- **Windows x64:** the native owner-device proof passed the complete workspace
  and native suites plus 35 lifecycle stages. A private hosted Windows x64
  acceptance run also passed all four matrix jobs. Its value-suppressed
  evidence recorded 18
  bundle members, 31 archive entries, four native release executables, four
  native shipped executables, exact start/status/restart/stop behavior,
  stopped-workspace integrity, foreign-listener preservation and bounded
  cleanup. The retained private evidence digest is
  `7d34097b73cfcdc9d8534ec1dbb0a49a6662d5bbc0f49d737ce1363d8fa38944`.
- **Windows ARM64:** the native owner-device proof ran with an ARM64 OS, process,
  Rust/MSVC toolchain and produced PE32+ `0xAA64` executables. The full native
  workspace, strict Clippy, exact bundle and lifecycle proof passed, including
  initialization, idempotent start, restart, stopped integrity,
  foreign-listener refusal and cleanup. Its retained private evidence digest is
  `8d93ecdeb3454713b900646152797137871953908152254e48037fbe1a921746`.

These records establish native implementation and lifecycle evidence. They do
not by themselves settle licensing, distribution, signing, installer,
security-reporting or support-policy decisions.

## Shared contract

Every future artifact must have its own deterministic archive and checksum
sidecar; use a target-specific launcher and executable paths; verify the exact
target in the canonical manifest; bind loopback-only lifecycle commands to one
workspace; preserve foreign listeners; and reject changed, missing, extra,
linked, wrongly typed or wrongly targeted material before operation.

POSIX artifacts use the native shell launcher and private per-user runtime
state (XDG runtime state where available on Linux). Windows implements a
native per-user control authority with retained reparse/hardlink/handle
verification, protected current-SID state and pipes, Job Object ownership and
bounded cleanup. Either exact Windows artifact can be assembled only on its
matching native host with the explicit `windows-native-acceptance-candidate`
build feature; that feature does not alter this matrix or make a
support/release claim. Each artifact's root
`migration-stream-local.exe` is byte-identical to the verified native
controller. PowerShell is build/proof orchestration only, not the runtime
launcher or an elevation requirement.

The Windows-only systems boundary adds exact locked `windows-sys 0.61.2`
bindings and its locked `windows-link 0.2.1` transitive edge. Their package
metadata declares `MIT OR Apache-2.0`; this records dependency metadata only
and does not select a licence for this repository. The selected evidence tool
can generate a deterministic lock-universe SBOM; shipped-closure analysis,
formal notices, and legal review remain release work.

## Intentionally later

Installers, code signing, notarization, auto-update, service/daemon mode,
hosting, authentication, telemetry and external connectors are outside this
portable-bundle increment. No bundle adds an external target connection or
write capability.
