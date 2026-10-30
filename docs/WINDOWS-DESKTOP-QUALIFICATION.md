# Native-pair Windows installer compilation qualification

This is a proposed manual engineering qualification, not an installation or
release workflow. It requires the separately reviewed Windows NSIS packager to
be integrated at the same exact source commit. The qualification-only branch
based on integration `8a813b3` deliberately does not copy that other candidate.
No job may proceed without the actual `scripts/package-windows-desktop.py`.

The new `windows-desktop-installer-qualification.yml` workflow must first exist
on the default branch before manual dispatch is available. A later, concrete
run approval must name the exact source SHA, two native builds and publication
of **two reader-accessible native transfer artifacts**. The required
`publish_native_transfers` boolean defaults to false. False admits no jobs.
A source review or green synthetic CI is not that execution approval.

## Fixed execution route

Two explicitly named native jobs use the existing closed native tuples:
`windows-2025` / x64 / `1.99.1-x86_64-pc-windows-msvc` and `windows-11-arm` /
ARM64 / `1.99.1-aarch64-pc-windows-msvc`. Explicit job names give the combine job
unambiguous artifact-ID/hash outputs instead of last-writer matrix outputs.
Both use the existing pinned checkout and setup-node actions. The native driver
is reused byte-for-byte, with Node 24.22.1, npm 12.2.1 and Rust 1.99.1; its
native OS/process/processor/Rust gates, clean-source and exact-attempt-one guards
remain authoritative. Online locked cache preparation precedes the existing
resolver-offline build. Resolver flags do not isolate OS network access.

The existing driver builds the same-source Runtime, document launcher and
engineering assembler, executes the existing metadata/identity/Verify modes,
assembles a layout, then uses the existing preview Pack and local Extract path.
Pack plus staged/final local replay add three fixed read-only Verify processes.
These are real product process/thread startups, not just file reads. No Start,
Open, server, workspace, browser or installer mode is selected. Each native job
has a 45-minute external ceiling; existing capture limits are post-return,
not hard process-output or real-time OS interruption guarantees.

Each native job publishes exactly the existing opaque stored ZIP and the
original qualification JSON. Both are copied into its fresh transfer directory,
with externally exported SHA-256 for each file. Native metadata is allowlisted;
private build logs, caches, source workspaces and compiler paths are not sent.
Upload uses the existing pinned action, archive mode, missing-file error,
no overwrite and one-day retention. These transfers are downloadable by public
repository readers even though they are not end-user delivery. If a later stage
fails, they remain explicitly unqualified until their ordinary expiry; there
is no automatic broad deletion.

The x64 combine job downloads only the two returned immutable artifact IDs,
using the existing pinned download action with digest mismatch fatal. Returned
artifact digests bind the action's two-file transport archives; they are **not**
the inner ZIP hashes. The original native job separately supplies inner ZIP and
receipt SHA values. Both download directories must contain exactly the two
fixed regular file names. The current native job trees and source snapshots
must agree before combining.

## Passive cross-architecture replay

`scripts/windows-desktop-transfer.py` handles only the existing stored ZIP
subset: bounded EOCD/central/local structures, no ZIP64/descriptors/encryption,
comments/extras/compression/preamble/trailing bytes, exact offsets and names,
ASCII safe member paths, no case aliases/traversal/ADS/device paths or special
file/link attributes. Limits are 1 GiB archive, 642 MiB expanded, 128 MiB member,
1 MiB receipt, 4097 entries and 66 archive path components. JSON duplicates,
nonintegral values, excessive depth/nodes and mismatched selected receipt
projection refuse. The original qualification receipt hash is an external
input, not a claim authenticated by its own JSON. Browser-zone metadata is
refused on Windows, never stripped.

The extractor checks every declared member byte count/hash and the exact
receipt/ZIP graph, writes only a fresh private stage with create-only files,
and publishes a new destination without replacing it on Windows. Partials are
preserved on failures. Ordinary Windows file identities are fresh; inode/SID,
ACL or generic Explorer/Unix attribute preservation is not asserted. The tool
is purpose-specific passive transfer validation, **not** Runtime verification.
It has no validator injection or executable invocation. In particular, ARM64
Runtime binaries are never run on the x64 combine runner.

The actual reviewed packager then independently validates both native receipts,
canonical Runtime manifests, full graph, every byte/hash and PE target, common
source commit/tree/snapshot/product version and frontend identity. It replays
source/copied/compiler/template/script identities around the real NSIS compile.
Those packager gates are not replaced by the transfer check.

## Existing compiler only; no install or execution of Setup

The compiler is fixed to the runner's existing
`ProgramFiles(x86)\NSIS\makensis.exe`. The x64 native job checks presence before
its build; combine fails if absent or if its concrete native file, NSIS 3 file
version or required six header/plugin resources cannot be observed. There is
no Chocolatey, winget, installer, dynamic download or alternate compiler path.
Runner availability and a sufficiently recent NSIS native-ARM detection header
remain real-run prerequisites, not locally established facts. The final bounded
metadata reports compiler hash, file version and six resource hashes plus Python
version/hash. These identify observed bytes; they do not prove supply-chain
provenance or a complete compiler-resource closure.

The packager performs one real `/NOCONFIG /V2 /INPUTCHARSET UTF8` compilation
with its bounded direct-child runner. There is no separate unbounded compiler
version subprocess. The combine job has a 15-minute external ceiling, so the
whole three-job workflow can exceed 45 minutes. Compilation metadata and a
fresh Setup SHA/byte count are the success witness, with
`COMPILED_PASSIVE_BYTES_ONLY`, `installerExecuted:false` and `setupUploaded:false`.
The produced Setup remains ephemeral; **no final installer artifact is uploaded**.
Native CPU auto-selection, actual install/uninstall/Open With, SmartScreen,
MOTW behavior and lifecycle remain unqualified. No paid services/signing,
registry changes, app launch or user's device are involved.

A downloadable final Setup is a separate future publication decision. No run or
publication is implied by this source candidate or by the older x64 preview
approval.

## Source-only checks

```sh
python3 -B scripts/windows-desktop-transfer-self-test.py
```

Eleven groups exercise actual passive code over tiny synthetic ZIPs and hostile
mutations, with subprocess creation blocked, plus fixed workflow/driver source
contracts. Windows source CI also parses the new PowerShell driver without
executing it. Local Mac tests do not qualify Windows publication semantics,
PowerShell parsing, NSIS availability/compilation, real artifact transport or
same-source native builds. Preserve first failures; no automatic retry.
