# Windows document-open coordinator source

`native/windows/document_open.rs` provides a callable coordinator for one
already-bound project document. It invokes the existing portable controller's
`open` command; it does not implement another Start, workspace or project core.
The module has no application entry point or automatically executed operations.

An integration owner supplies the trusted intact bundled runtime root, a bounded
controller runner and a browser presenter. The controller executable is always
that root's `libexec\migration-stream-local-runtime.exe`. No environment,
current-directory, PATH or document-directory lookup selects an executable.
The runner receives exactly `open`, `--` and the selected document as three
separate native arguments. It receives a 45-second deadline and 65,536-byte cap
for each output stream. A real runner must enforce those bounds during capture,
drain both streams and bound cleanup of its directly owned controller child.
A concrete `WindowsProcessRunner` is now available for explicit Windows composition;
its native process behavior still requires separate Windows validation.

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
Selection does not initialize, adopt, bind or repair a project. A future native
entry point and exact-artifact acceptance remain prerequisites;
the current runtime target and core gates remain unchanged.

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
