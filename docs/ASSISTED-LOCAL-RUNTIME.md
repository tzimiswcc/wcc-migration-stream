# Migration Stream assisted local runtime

This bundle is the first bounded practitioner runtime for Migration Stream. It
runs the real local workbench on one explicitly selected workspace without a
repository checkout, Rust, Node/npm, Git or Claude.

The currently accepted artifact is Apple-silicon Mac (`macOS 26`, `arm64`,
`aarch64-apple-darwin`). The acceptance host is macOS 26.5.2. The source also
defines separate exact-target portable-bundle entries for Windows x64/ARM64
and Linux x64/ARM64. Those are not universal binaries and remain
**native-runner/device required** until their exact host proof has passed; a
cross-build/static check is not a support or release claim.

Portable bundles are not installers. Signing/notarization, Windows code
signing, auto-update, daemon/service installation, authentication and network
hosting remain later release work.

The bundle checksum proves accidental-change detection, not publisher
signature or notarization. Loopback binding reduces exposure; it is not
authentication. Use only synthetic or appropriately governed local data.

## Unpack and verify

Keep the ZIP beside its `.zip.sha256` file. In Terminal:

```sh
cd /absolute/path/containing/the/download
/usr/bin/shasum -a 256 -c migration-stream-local-macos-arm64.zip.sha256
/usr/bin/ditto -x -k migration-stream-local-macos-arm64.zip /absolute/private/runtime
cd /absolute/private/runtime/migration-stream-local-macos-arm64
./migration-stream-local verify
```

The unpacked directory is immutable runtime material. Do not put a workspace,
logs, received data or evidence inside it. If verification reports a changed,
missing, extra, linked, wrongly typed, wrongly permissioned or wrong-platform
member, stop and obtain a known bundle again.

## Operate one workspace

Create or select one existing real directory outside the bundle. The browser
never supplies or learns an arbitrary server path.

```sh
mkdir -m 700 /absolute/private/workspaces/customer-readiness
./migration-stream-local start \
  --workspace /absolute/private/workspaces/customer-readiness \
  --port 4173
```

Open the exact loopback URL printed by `start`. A fresh empty directory opens
the Object Workspace initialization screen. An established workspace reopens
its existing identity and evidence.

Use the same explicit workspace and port for lifecycle commands:

```sh
./migration-stream-local status \
  --workspace /absolute/private/workspaces/customer-readiness \
  --port 4173

./migration-stream-local restart \
  --workspace /absolute/private/workspaces/customer-readiness \
  --port 4173

./migration-stream-local stop \
  --workspace /absolute/private/workspaces/customer-readiness \
  --port 4173

./migration-stream-local integrity \
  --workspace /absolute/private/workspaces/customer-readiness \
  --port 4173
```

`integrity` requires that this workspace runtime is stopped. Every command
re-verifies the immutable bundle first. `status` distinguishes the exact owned
runtime, stopped or stale session evidence, and a foreign listener. Lifecycle
commands never replace or kill a foreign listener.

Private runtime logs and session controls are outside both the immutable
bundle and governed Object Workspace. POSIX uses the current user's private
mode-0700 runtime directory. The staged Windows implementation derives its
root from the LocalAppData Known Folder API and requires a protected,
owner-only current-SID DACL on its root, session, lock and control state. These
records are local diagnostics/control authority, not migration evidence.

## Authority boundary

The bundle supplies the existing Object Workspace, Destination, Source,
Mapping, Validation, Compare, Issues, Decisions, Reconciliation and local
Readiness Package journey. Existing profile, mapping, validation, decision,
reconciliation and package gates are unchanged. A controlled pack supplies
reviewed-draft profile material and becomes local mapping/validation authority
only after separate approval of its exact profile hash. Imported EDMX remains
technical evidence until a practitioner freezes and separately approves an
exact reviewed destination profile.

From a completed validation, the workbench can also create an explicitly
authorized local remediation ZIP and accept repeatable immutable source
revisions in the same engagement. Revision comparison remains value-suppressed;
remediation key and optional offending values stay only in the exact verified
ZIP. Compatible revisions replay the exact approved profile and mapping
authority. Structural drift requires a new mapping draft, prior evidence stays
immutable, and a revised single-entity run begins fresh downstream governance.
When a declared key is incomplete, comparison uses a private full-row multiset
fallback and states that basis without exposing values; an edit may therefore
appear as one CLOSED plus one NEW finding rather than an asserted row lineage.

No external-system connection, target write, external transmission, deployment or public
release capability is added by this bundle. All current multi-entity work
remains read-only after First Issues/Compared Validation; multi-entity
Decisions, Reconciliation and Package authority are not implemented.
