# Native-pair Windows installer compilation qualification

This is a proposed manual engineering qualification, not an installation or
release workflow. It requires the reviewed Windows NSIS packager at the same
exact source commit as both native builds. No job may proceed without the
actual `scripts/package-windows-desktop.py`; transfer metadata does not
substitute for that shared production validator.

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

## Explicit portable compiler; no install or execution of Setup

Source CI and this qualification use the same reviewed
[`prepare-nsis-tool.py`](../scripts/prepare-nsis-tool.py) contract: a fixed
official portable NSIS 3.13 ZIP URL/hash, one bounded fetch, hash before safe
create-only extraction, complete graph/member-byte and receipt validation, and
the direct `nsis-3.13/Bin/makensis.exe` compiler. The installed Program Files
assumption is removed; the failed image inventory documented no NSIS path.
Native build jobs need no unused compiler precheck. Preparation occurs only on
the x64 combine job; source CI independently prepares the same distribution
before its tiny synthetic template compilation. This is explicit tool
preparation, not a fallback after another compiler fails.

The [Windows packaging guide](WINDOWS-DESKTOP-PACKAGING.md) records exact archive
and direct-compiler identities, limits and official resource-root source. No
installer, admin action, package-manager mutation, PATH search, global config or
arbitrary tool/version URL is admitted. The existing bounded packager runner
executes the direct compiler and retains its cleared environment and
`/NOCONFIG` policy. It does not invoke the child-spawning release-root stub.
The preparation helper itself invokes no executable. Archive integrity and all
copied resource bytes are revalidated after compilation; their hashes bind
observed bytes, not publisher authentication or a supply-chain attestation.
The final metadata includes the archive/preparation-receipt hashes, receipt-bound
NSIS version and its `PINNED_ARCHIVE_RECEIPT` evidence type, compiler hash, nine
pinned member hashes and Python version/hash. PE file-version metadata is
optional and absent/null for this direct compiler, which has no `RT_VERSION`.
It neither gates qualification nor triggers an executable version probe or a
switch to the root stub. Any
preparation/compile failure stops once and preserves explicitly unqualified
transfer artifacts until their one-day expiry; no retry or broad deletion.

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

Twelve groups exercise actual passive code over tiny synthetic ZIPs and hostile
mutations, with subprocess creation blocked, plus fixed workflow/driver source
contracts. Windows source CI also parses the new PowerShell driver without
executing it. Local Mac tests do not qualify Windows publication semantics,
PowerShell parsing, portable NSIS preparation/compilation, real artifact transport or
same-source native builds. Preserve first failures; no automatic retry.


## One fixed old native pair: proposed Combine-only lane

`windows-combine-fixed-inputs.yml` is manual/default-off, main-only and first-attempt
only. It requires a separately approved exact tooling SHA and one15-minute run.
It admits only original run41057681963's two fixed artifact IDs, hashes, native
jobs and a61 product commit/tree/snapshot. The earlier native builds are not
relabeled with the newer diagnostic tooling commit. Both identities appear in
final evidence. Source admission refuses any changed product, transfer, packager
or NSIS template; only the explicit reviewed diagnostics/lane paths may differ.

Admission checks producer metadata and requires20minutes remaining before BOTH
fixed expiries (earliest2026-10-31T12:55:37Z), before downloads and again before
Combine. Missing/expired/deleted/changed inputs refuse; no fallback or rebuild.
This deliberately time-limited lane is not a generic artifact reuse service.
Existing pinned exact-ID downloads, passive archive/receipt/PE/graph checks,
12GB disk/8GB memory gates and pinned portable NSIS preparation remain. Only x64
compiler runs; ARM payload is passive. Setup remains ephemeral and is never run
or uploaded. No installer/app/runtime, registry or device operation is admitted.

Source/synthetic tests do not approve a run. The workflow must first be reviewed
and present on the default branch; a fresh concrete execution decision remains
required. Prior native qualification grants are consumed. Command failures may
still refuse; reuse does not establish or repair the old Combine failure cause.


### Corrected passive-reader tooling identity

The old passive reader incorrectly compared Windows path creation-time `ctime`
with descriptor change-time `ctime` on Python3.12. Corrected tooling reuses the
reviewed packager's cross-interface identity binder and independently preserves
both full freshness fingerprints, including each interface's own `ctime`, before
and after reads. Archive and receipt path/descriptor drift still refuse before
publication. This deterministic verifier defect is consistent with the failed
Combine attempt; remaining real-input guards are unqualified.

Reuse admission now explicitly binds the exact original a61 transfer/test blobs
and modes to the exact reviewed corrected tooling blobs and modes. It does not
allow arbitrary verifier updates or change old product provenance. Runtime,
packager, NSIS template and Cargo/product sources remain byte-identical. The
revised grant, after review, would select this corrected tooling commit while
retaining original product a61/run41057681963 and every input hash. Existing
attempt41079139105 is consumed; source tests do not authorize a new run.

Both Windows source lanes run tiny synthetic passive transfer tests and one
fixed Python transfer CLI over newly generated temporary fake ZIP/JSON only.
They never invoke a Runtime, controller, app, installer or product binary. Local
portable tests model named/descriptor views independently; actual Windows native
file semantics require the hosted changed-head source check.
