# Windows document-open coordinator source

`native/windows/document_open.rs` provides a callable coordinator for one
already-bound project document. It invokes the existing portable controller's
`open` command; it does not implement another Start, workspace or project core.
The callable module has no automatically executed operations. The manual
engineering entrypoint described below supplies the source composition.

An integration owner supplies the trusted intact bundled runtime root, a bounded
controller runner and a browser presenter. The controller executable is always
that root's `libexec\migration-stream-local-runtime.exe`. No environment,
current-directory, PATH or document-directory lookup selects an executable.
The runner receives exactly `open`, `--` and the selected document as three
separate native arguments. It receives a 45-second deadline and 65,536-byte cap
for each output stream. A real runner must enforce those bounds during capture,
drain both streams and bound cleanup of its directly owned controller child.
A concrete `WindowsProcessRunner` is now available for explicit Windows composition;
its native process evidence is limited to the separate synthetic fixture lane
below, rather than a packaged application or runtime acceptance.

`WindowsPath` retains UTF-16 units without UTF-8 conversion, including unpaired
surrogates. Its Windows-only native constructor uses `OsStrExt::encode_wide`.
The optional Windows-only `ControllerInvocation::command()` constructs a native
`std::process::Command` with separate `OsString::from_wide` arguments, cleared
environment, null stdin and piped output. It neither spawns nor waits. There is
no shell, PowerShell command text, manual quoting or `raw_arg`. The existing
portable controller remains responsible for descriptor, filesystem, bundle,
workspace, owned-runtime and listener validation.

The accepted lexical subset is a drive-absolute path below a root or a UNC path
below a share, using native backslashes. Relative, device/extended namespace,
forward-slash, empty-component, dot-component, trailing-space/dot, embedded NUL,
control, wildcard and alternate-stream forms refuse without normalization.
This is not a filesystem or alias/reparse-point validation claim. The supplied
runtime root must already be trusted; a selected document cannot supply it.

Exactly one document is required. Calls require exclusive mutable coordinator
access. After a validated controller success the exact document units remain
pinned, including when browser presentation fails. Reopening that exact path
delegates to the same controller; a different spelling/path refuses. There is
no object switching, Stop, token extraction or automatic retry.

The coordinator accepts only exit zero plus the existing CLI v1 and status v2
JSON envelopes with `ok:true`, classification `OWNED`, listen address
`127.0.0.1:4173` and URL exactly `http://127.0.0.1:4173`. Both streams must fit
the cap. Malformed, duplicate required fields, unsuccessful or mismatched
responses refuse before browser presentation. Only a fixed typed loopback
destination reaches the browser callback; controller-supplied arbitrary URLs,
paths and raw diagnostic text are never presented. The response is a trusted
controller report, not independent runtime authentication by this adapter.

Run the new synthetic source proof with the repository's existing toolchain:

```sh
cargo test --locked --test windows_document_open
```

Tests call the actual coordinator with injected operations and synthetic
envelopes. They preserve Windows UTF-16 identities/arguments, including UNC,
spaces, metacharacters and unpaired surrogates, and cover cardinality, document
pinning, controller/browser failures, exact ownership/loopback validation,
malformed responses and output limits. They launch no controller, browser,
runtime, native document event or network service. A non-Windows test run does
not compile or qualify the Windows-only native `Command` projection.
The manual source workflow adds a separate Windows-hosted job for this same
synthetic target and strict Clippy. Two Windows-only tests construct native
`OsString`/`Command` values and read their paths/arguments without spawning.
Environment inheritance clearing and stdio policy remain static builder checks;
an empty explicit environment iterator alone does not prove inheritance policy.

No registry/file association, installer, package, signing, release, native
Windows runner/device acceptance or platform-support claim is included.
Selection does not initialize, adopt, bind or repair a project. The manual engineering
entrypoint below does not establish exact-artifact acceptance; the current
runtime target and core gates remain unchanged.

## Bounded controller process owner

On Windows, explicitly passing `WindowsProcessRunner::default()` supplies the
blocking runner. It reuses the existing native `Command` builder unchanged:
fixed trusted executable, exactly three native UTF-16 arguments, empty inherited
environment, null stdin and separately piped output. It has no browser or entry
point. Callers must execute this blocking operation off their UI thread.

The owner polls each pipe through the existing Windows native boundary's narrow
`ChildStdout`/`ChildStderr` helpers. Each poll reads at most 4,096 currently
available bytes, with one exclusive reader and no blocking EOF reader threads.
Both streams are serviced before the child exit poll. Each stream admits at most
65,536 bytes; overflow, read/poll error or the 45-second deadline refuses without
returning captured text. Both pipe closure and known child exit are required for
success. A descendant retaining a pipe cannot create an unbounded EOF join.
The supplied runner limits must equal the coordinator's fixed limits.

On failure the owner requests termination of only its directly retained
controller child and polls for known exit for at most two more seconds. Unknown
exit is never treated as death. Failed cleanup retains that child handle and
refuses later spawns on this owner. Dropping an unresolved owner makes one final
direct-child termination request without waiting; this does not prove exit.
It never kills a Job/process tree or a PID from JSON: the controller may have
started a legitimate runtime whose lifecycle authority remains in the core.
No controller result or OS error text is exposed by this owner.

These are application-level polling budgets, not real-time OS guarantees.
Windows process creation, pipe syscalls, termination and scheduling may exceed a
budget while an OS call is in progress; a deadline begins before spawn but cannot
interrupt that call. The trusted runtime path is an integration prerequisite,
not newly verified or filesystem-pinned by this runner. No claim of bounded OS
failure recovery, descendant termination or orphan absence follows.

Focused pure source checks (no child processes, controller, browser or runtime):

```sh
cargo test --locked --test windows_document_open process_owner_tests
cargo clippy --locked --test windows_document_open -- -D warnings
```

The fake child and virtual clock exercise the same capture/cleanup state machine,
including both byte caps, fair draining, unknown poll/read failures, inherited
open pipes, exact UTF-16 invocation and retained cleanup failure. A Windows-only
construction check creates the owner/coordinator without spawning. Non-Windows
proofs do not compile or qualify `PeekNamedPipe`, native reads, `Command::spawn`,
`Child::kill` or the complete Windows process owner. Native synthetic-process
verification and eventual exact-artifact acceptance remain separate work.

## Separate synthetic native process proof

`windows_process_owner_native` is an explicit `harness=false` test target, not a
product binary. On non-Windows hosts it prints an explicit skip. The Windows
source workflow runs it with a three-minute external step timeout:

```sh
cargo test --locked --test windows_process_owner_native
```

The test copies only its own test executable into a fresh current-user protected
synthetic directory, including the exact fixed runtime helper name. A contained
case driver obtains the existing coordinator's native invocation and calls the
actual process owner. The helper accepts only `open`, `--`, one synthetic path;
that path contains spaces, Unicode, metacharacters and an unpaired UTF-16 unit.
No real project document or product executable is opened.

Six cases cover native arguments/environment clearing/observed stdin EOF, simultaneous
65,536-byte stdout and stderr, overflow on each stream, known exit 17 and one
actual 45-second timeout. The driver inherits one fixed synthetic environment
sentinel; the helper must not inherit it. Nothing enumerates environment values.
The case driver's explicit environment contains only that sentinel and the
verified `SystemRoot` supplied by the existing retained `system_root_directory()`
capability, replayed around admission. Missing/invalid capability refuses; no
ambient environment fallback or value logging is used. The native Job guard
requires this entry before process creation; sentinel-only admission cannot run.
The Job already gives the driver NUL stdin, so helper EOF does not distinguish
explicit null stdin from inherited NUL. The unchanged command source confirms
`stdin(Stdio::null())`; this fixture does not uniquely discriminate that policy.
Overflow helpers remain alive after write errors; timeout/overflow helpers have
no voluntary exit before their 50-second self-expiry. They deliberately retain
an exclusive synthetic file handle until process exit. The driver must observe
its release before outer Job cleanup; timeout refusal and that cleanup must
occur before 49.5 seconds, so the fail-safe alone cannot satisfy the test.

Each driver is assigned suspended to an existing kill-on-close Job before
resuming; nested-Job refusal never falls back to an uncontained launch. Its
60-second watchdog and five-second cleanup act only on retained synthetic Job
and process handles. The Job cannot turn failed owner cleanup into a passing
case because the driver validates first. Failed evidence directories remain
private for diagnosis. OS calls and runner scheduling still have no hard
real-time guarantee; unknown cleanup fails. This proves only these trusted
synthetic process behaviors, not arbitrary descendant/orphan absence, a real
runtime, browser, native document event, installer or supported platform.

Fixed diagnostic stages identify Job creation/admission, exact driver exit code,
driver startup/path/environment/invocation, owner return, readiness, exclusive
handle release and assertions. Driver records are create-only eight-byte files
under its owned synthetic case directory; outer readback admits only nine fixed
stage names and eight-byte versioned records. Logs contain fixed case/stage/category
labels and numeric OS/error-kind/exit codes, never paths, argv, environment values,
controller output or raw error text. Diagnostic time remains inside the existing
acceptance bounds; failed diagnostics cannot turn a case into a pass.

## Manual engineering launcher source

`migration-stream-document-open` is a manual console engineering launcher. Its
source composition is exactly one native document operand, the retained existing
coordinator, `WindowsProcessRunner`, and fixed-loopback Shell submission. It
accepts no flags, runtime-root/command/port override or environment/cwd/PATH
fallback. It adds no picker, registry association, installer or bundle staging.
The existing runtime builder continues to stage its existing four executables.

The explicit engineering layout is a launcher directory containing
`migration-stream-document-open.exe` and an intact adjacent `Runtime` directory;
the controller remains `Runtime\libexec\migration-stream-local-runtime.exe`.
The launcher derives that layout only from `current_exe`, accepting an ordinary
fully qualified drive spelling and exact launcher basename. Extended/device/UNC
executable namespaces refuse without prefix stripping; document lexical rules
remain unchanged. Retained `RealFile`/`RealDirectory` checks refuse reparse or
invalid metadata and remain alive through controller/browser submission. These
checks are not publisher authentication or atomic spawn-image pinning. Operator
trust in both launcher and runtime is a precondition.

The narrow native browser function takes no URL or path. It initializes COM STA
with `COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE`, refuses initialization
failure/changed mode, and balances S_OK/S_FALSE on the same thread. It calls
`ShellExecuteExW` with fixed `open`, fixed `http://127.0.0.1:4173`, null parameters,
and `SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI`. No ASYNCOK, NOZONECHECKS, process
handle or fallback is requested. All UTF-16/struct storage survives API return.
The cached windows-sys Registry feature is enabled only because it gates this
structure/function binding; the launcher performs no direct registry operation
or registration change.

Microsoft documents that NOASYNC does not apply to URIs and that NO_UI exempts
security prompts. API success therefore means submission only: URI handoff
survival after immediate console exit and browser rendering remain unqualified.
There is no arbitrary delay, security bypass, off-thread handoff or invented
completion signal. Failure output is fixed and omits document paths, OS errors
and controller text. No native launch or platform-support claim follows.

References: [ShellExecuteExW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw),
[SHELLEXECUTEINFOW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-shellexecuteinfow),
[CoInitializeEx](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-coinitializeex).

Focused pure composition and COM policy tests use injected operations only:

```sh
cargo test --locked --test windows_document_open engineering_composition_tests
cargo test --locked -p migration-stream-windows-native browser_submission_tests
```

They execute no product/helper/launcher, COM, browser, picker or project process.
Non-Windows runs do not compile the real retained-handle/Shell projection; Windows
source compilation and eventual exact-artifact evaluation remain separate.

## Create-only engineering layout assembler source

`migration-stream-windows-engineering-layout` is a build-time console tool for
already-built, operator-trusted artifacts on native Windows. Required arguments
are `--launcher`, `--expected-launcher-sha256` (64 lowercase hexadecimal digits),
`--runtime`, `--output-directory`, and
`--engineering-policy local-unsigned-engineering`. Paths must be absolute;
the output parent must already exist and the output itself must be absent.
Inputs and output must not overlap. There are no execution or verifier-bypass
flags. No launcher, controller, browser, installer or registration is run.

The assembler itself must carry the same compiled product commit, source state,
source snapshot and frontend asset identity as the runtime: the existing direct
bundle verifier retains this provenance gate. A generic metadata-free build
refuses. The existing runtime build driver and its manifest remain unchanged.
The caller must retain independent build provenance for the outer launcher;
its expected SHA-256 and matching Windows PE64 architecture bind bytes and
architecture only, not a compiler, publisher, signature or minimum OS version. PE64
validation uses the exact bounded immutable launcher-byte capture that is hashed
and copied; an earlier separate path read cannot authorize different bytes.

The output contains only `migration-stream-document-open.exe` and an intact
`Runtime` subtree. The document launcher digest is reported separately from the
runtime manifest's platform-launcher member digest. Every runtime verification
field must match across source, stage and published output except the relocated
bundle root. Runtime files, manifest and graph are copied without rewriting.

Capture streams directory enumeration with limits of 4096 entries (including
root), 64 directory levels, 512-byte relative paths, 128 MiB per file and
512 MiB plus 1 MiB manifest allowance in total. Source identities, bytes, modes
and graph are replayed before publication. On Windows, existing retained path
checks reject reparse ancestors and hard-link aliases; Unix mode semantics are
not approximated on Windows. These checks are not atomic spawn-image pinning.

A uniquely owned stage is created in the output parent and published with the
existing no-replace directory protocol. An existing output is never overwritten.
Only a provably owned unpublished stage is eligible for cleanup. Windows
intentionally retains a partial when safe recursive deletion cannot be proved;
inspect that private partial manually. After a move or ambiguous publication
failure, visible output is preserved, not automatically removed. Successful
assembly is not signing, installation, execution, compatibility, release or
platform-support qualification.

Focused source verification uses `cargo test --lib windows_engineering_layout_`.
Tests use temporary synthetic PE headers, canonical synthetic runtime manifests
and a private injected verifier seam; no packaged program is executed. Windows
hosted tests and production library/new-bin lint must be reported separately
from local non-Windows source checks.

## Manual ephemeral Windows build qualification source

The separate `windows-engineering-layout-qualification.yml` workflow is manual
only. It requires the exact reviewed source commit as an input and checks it
against both the event SHA and checkout. A mismatched event/attempt skips the
job before tool setup. GitHub requires this workflow to exist
on the default branch before dispatch; once present, a reviewed branch can be
selected with `--ref`. See [GitHub's manual-run contract][manual-workflow]. Source
review, merge, and earlier synthetic CI do not qualify this new build workflow
or admit an executable run. The first real run needs a separate concrete
execution admission after review.

The intended lane is one ephemeral `windows-2025` x64 job, with Node 24.22.1,
npm 12.2.1 and Rust 1.99.1. It selects existing runner Python 3.10 or later in
the 3.x series and records its actual version and byte hash; it does not install
or infer a product Python requirement. Native architecture checks, at least
12 GiB free on the temporary drive and 8 GiB physical memory precede building.
These are admission thresholds, not enforced target/cache disk or memory quotas.
The 45-minute job timeout is an external ceiling, not a per-child interruption
or cleanup proof.

Online preparation installs the pinned toolchain/npm into the ephemeral runner
and fills locked Windows npm/Cargo caches. npm cache preparation disables
lifecycle scripts; the existing offline driver later runs normal locked build
scripts. Resolver-offline npm and Cargo flags are applied only after cache
preparation. They do not provide OS-level network isolation or prevent arbitrary
third-party build-script networking. No provider/model call is selected.

`-RetainEngineeringTools` is an opt-in builder flag requiring
`-RetainedBuildRoot`. It adds exactly the document launcher and engineering
assembler to the existing private single-copy/hash/length/PE checks, preserving
the 128 MiB member and 512 MiB aggregate staging bounds. Without the flag, the
existing four-file selection is unchanged. These tools remain outside Runtime;
its manifest, source/compiled identity quartet and mandatory gates are unchanged.

The driver still starts the controller with `__build-identity`, the kernel with
`__bundle-build-identity`, the workbench with `--bundle-build-identity`, and the
controller's Windows platform-launcher copy with `verify`. These metadata and
file-verification modes include product process startup and, for async entry
points, Tokio thread startup. They select no Start/Open/server/project/browser
operation. Their existing `Command::output` collection checks size after return
and has no individual deadline; the workflow does not claim capped streaming
child capture or an environment sandbox.

The existing driver builds all binaries once from the same exact commit,
source snapshot, frontend snapshot and native target, using `-NoArchive`. Fresh
private copies of the two engineering tools are compared with their Cargo
outputs. The engineering assembler then receives the copied launcher's expected
hash, the intact runtime and a nonexistent output. It directly verifies source,
stage and final Runtime files without starting another product executable. The
workflow checks its actual result schema, compiled source identity, target,
counts, both manifest-file hashes, outer launcher bytes and exact two-entry
outer layout. Full member/hash/graph verification remains in the existing
production verifier rather than a new workflow manifest-admission copy.

The bounded evidence contains only fixed keys, public source identities,
versions, hashes, counts, stage and numeric exit status. Custom command logs,
paths, environment and exception objects stay private to the ephemeral runner;
standard GitHub action/runner setup logs retain their platform behavior. This is
not a general log-redaction or environment-sandbox claim. Logs and evidence use
create-only reservations; the first failure stops without an automatic retry.
Re-run attempts refuse; any later qualification uses a separately admitted fresh
manual run. With the preview input left false, no binary, layout, private log or cache is uploaded. Outputs
are left for ephemeral runner teardown, preserving failed partials for the job's
remaining lifetime. There is no output launch, browser/COM/Shell, OS registry,
association, product installer, signing, release or support promotion.

Hashes identify bytes; they do not authenticate a compiler, publisher or build
provenance. The fixed command chain, recorded source/build inputs, selected tool
hashes and copy checks form an external operator build receipt, with the runner
image recorded when available. Metadata-free or mismatched assembler builds
still refuse. Source-contract checks run with
`python3 -B scripts/windows-engineering-qualification-self-test.py`; these are
static mutation regressions, not PowerShell parsing/execution or real-artifact
qualification.

[manual-workflow]: https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow


## Optional unsigned engineering candidate transport

The manual qualification workflow adds an explicit `publish_engineering_preview`
boolean, defaulting to false. Exact string parsing admits only `true` or `false`;
helper execution and both artifact actions have separate true/first-attempt/exact
source guards. Existing qualification gates and the default no-upload path remain.
A future real build, archive, upload and download replay need separate concrete
execution approval; source review and synthetic CI do not admit that operation.

The purpose-only `scripts/windows-engineering-preview.ps1` accepts `Pack` with
explicit `-Layout`, `-BuildReceipt` and create-only `-Archive`, or `Extract` with
`-Archive`, trusted external `-ExpectedArchiveSha256` and nonexistent
`-Destination`. Production commands have no verifier-skip option. The caller must
already trust the same-source engineering build; SHA-256 identifies bytes rather
than publisher, compiler or signing provenance. Extraction checks the closed ZIP
hash before opening its member catalog. The ZIP's bounded receipt binds public
build identities and the inner layout, without a self-referential ZIP hash.

The container holds `engineering-layout/` and `preview-receipt.json`. The layout
retains exactly the outer launcher and intact `Runtime` subtree. The reference
route uses a strict stored ZIP subset: no compression, Zip64, descriptors,
comments or extra fields. It bounds the archive to 1 GiB, expanded contents to
642 MiB, each member to 128 MiB, receipt to 1 MiB and layout to 4096 entries with
bounded depth and names. It refuses ambiguous/case-colliding names, traversal,
Windows device/drive/UNC/ADS paths, links, reparse points, special entries,
overwrite and overlap. Fresh extraction stages beside the destination and moves
without replacement; failed partials and visible outputs are preserved rather
than recursively deleting uncertain ownership.

Every graph entry and file length/hash is replayed. The fixed Runtime launcher
runs read-only `verify` before packing and after each local/download extraction;
its actual verification identity must match except the relocated bundle root.
The outer launcher digest stays distinct from Runtime's platform launcher and
controller digests. These three additional Verify invocations start product
processes/threads, selecting no Start/Open/server/project/browser operation.
Their post-return capture checks share the existing finite job timeout; this is
not a streaming capture or OS network-isolation claim.

The upload uses pinned `actions/upload-artifact` v7 in single-file
`archive: false` mode, seven-day retention, missing-file failure and no overwrite.
Its filename explicitly says **engineering candidate**. Pinned
`actions/download-artifact` v8 selects only the returned artifact ID, preserves
the opaque ZIP with `skip-decompress: true` and makes digest mismatch fatal.
Only a successful trusted-digest check and create-only reference extraction with
production verification emits `QUALIFIED_ENGINEERING_PREVIEW`. An earlier upload
is unqualified on any later failure and may remain for its seven-day lifetime;
the workflow does not delete other artifacts or announce a qualified preview.

Windows readonly/archive/normal attributes are not Runtime verification gates;
this route does not restore attributes, ACLs, SIDs or original file identities.
It does not promise Explorer/Unix-mode fidelity. It refuses a ZIP carrying a
`Zone.Identifier` stream instead of removing or silently discarding that marker.
Browser downloads, MOTW propagation, SmartScreen, URI completion, application
launch, installer behavior, signing and public platform support remain unqualified.

Windows source CI parses the helper and test script, then runs only tiny fake
ZIP fixtures via `scripts/windows-engineering-preview.Tests.ps1`. The private
fixture seam injects a synthetic verifier; no product executable, real Runtime,
browser, build or artifact publication is selected. Local Python contract tests
protect default-off/action/extraction guards. Actual PowerShell parsing and ZIP
behavior require the Windows source run; static Python/YAML checks alone do not
prove them.
