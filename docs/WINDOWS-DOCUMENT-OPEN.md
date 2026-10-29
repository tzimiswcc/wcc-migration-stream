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
This source does not yet supply or qualify that Windows process owner.

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
entry point/process owner and exact-artifact acceptance remain prerequisites;
the current runtime target and core gates remain unchanged.
