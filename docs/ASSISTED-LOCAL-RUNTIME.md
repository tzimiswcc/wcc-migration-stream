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

## Create and reopen one project

From the verified bundle directory, select a new object directory outside the
bundle whose parent already exists. The object directory itself must not exist;
do not create it with `mkdir` first.

```sh
./bin/migration-stream project create \
  --directory /absolute/private/projects/customer-readiness \
  --display-name "Customer readiness" \
  --initialize-and-bind \
  --migration-object-name-or-scope "Customer readiness" \
  --creator-attribution "Local operator"

./bin/migration-stream project inspect \
  --project /absolute/private/projects/customer-readiness/project.mstream

./migration-stream-local start \
  --project /absolute/private/projects/customer-readiness/project.mstream \
  --port 4173
```

`--initialize-and-bind` explicitly initializes this new empty object's workspace
and binds its project descriptor. Supply the object's scope and creator
attribution; these fields grant no profile approval authority. After success,
use the returned `descriptorPath` for inspection and runtime commands. Inspect
is read-only. Open the exact loopback URL printed by `start`.

The existing controller also accepts a single-document open invocation for a
trusted caller. Pass one final bound descriptor after `--` so its native path
remains one argument, including spaces or shell metacharacters:

```sh
./migration-stream-local open --port 4173 -- "/absolute/private/projects/customer readiness/project.mstream"
```

`open` validates the same bound project and delegates to the existing verified
runtime start. The port defaults to 4173. It returns the existing runtime status
and loopback URL; it does not launch a browser. A pending, missing, altered,
linked or mismatched descriptor is refused without initialization or binding.
It accepts one project document, no workspace alternative or metadata overrides.
Owned-runtime reuse, writer isolation and foreign-listener refusal remain the
existing lifecycle's responsibility. This source entry point prepares a future
file-open adapter; it does not register an OS file association, provide an
installer or establish native file-open qualification or platform support.

Stop, then reopen the same object using its final descriptor and the same port:

```sh
./migration-stream-local stop \
  --project /absolute/private/projects/customer-readiness/project.mstream \
  --port 4173

./migration-stream-local start \
  --project /absolute/private/projects/customer-readiness/project.mstream \
  --port 4173
```

Reopening retains the existing workspace identity and evidence. `status`,
`restart` and `integrity` also accept `--project`; select either `--project` or
`--workspace` for each command. The lifecycle controls described below apply
to both selections.

Creation refuses every existing target directory, including an empty directory
or a previous partial attempt. To reopen a completed project, inspect its final
`project.mstream` and start it as above. If creation fails after retaining
material, read the failure's `project.phase`, known `descriptorPath` and
`nextAction`, and inspect the indicated descriptor before taking an explicit
recovery step. Leave the retained material intact; repeating `create` does not
resume, repair or overwrite it. A publication failure can leave a final
descriptor visible. A `project.pending.mstream` receipt requires explicit
workspace initialization and binding before runtime use; it cannot substitute
for the final descriptor. Omitting `--initialize-and-bind` deliberately leaves
that staged setup pending.

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

## Source-only macOS document adapter

`native/macos/DocumentOpenAdapter.swift` is an `NSApplicationDelegate` adapter
for a future native wrapper. Its caller must provide the trusted intact runtime
bundle subtree. It accepts one local document URL per event and invokes only
the fixed `libexec/migration-stream-local-runtime` executable with
`open`, `--`, and the exact losslessly represented document path. It clears the
child environment and accepts no port or workspace override.

Events are serialized; an in-flight request is refused. After a validated
successful controller response, a different document is refused rather than
switching objects. Repeated opens of the same document use the existing
controller and its ownership guards. Browser presentation is allowed only for
the exact successful CLI/status schemas, `OWNED` classification and
`http://127.0.0.1:4173` address. The runner works off the UI actor, caps each
output stream at 64 KiB and waits at most 45 seconds plus bounded cleanup.

This is source preparation only: it creates no `.app`, `Info.plist`, document
UTI, OS association, installer, signing identity, minimum-OS decision or
platform-support claim. The existing bundle verifier and lifecycle are unchanged.
The caller's native wrapper and real document events still require separate
implementation and exact-artifact acceptance.

With an existing macOS AppKit SDK and Swift 6 compiler, run the source proof
suite without starting an application, product controller or browser:

```sh
xcrun swiftc -swift-version 6 -warnings-as-errors \
  native/macos/DocumentOpenAdapter.swift \
  tests/native/macos/DocumentOpenAdapterTests.swift \
  -o /absolute/private/temporary/document-open-adapter-tests
/absolute/private/temporary/document-open-adapter-tests
```

Nine proof groups call the real handler with injected process/browser/error
functions. Seven additional cases exercise the unchanged production Foundation
`Process` runner against a private copy of the test executable acting only as a
synthetic helper, with browser/error presentation injected. Helpers self-expire
after 50 seconds, and the whole test executable has a 70-second fail-safe. The
suite includes one real 45-second deadline and bounded cleanup case, output
caps, pipe backpressure, exact native arguments and a fixed sentinel checking
that a parent-only environment variable is absent. It removes its own
private fixture files. No product controller, runtime, browser, native document
event or network service is launched. These are source-process proofs, not
native association or installer acceptance; Linux source CI does not compile
or qualify AppKit.

## Internal source composition for Open Object

`native/macos/ApplicationBootstrap.swift` adds an explicit, uninvoked
`ApplicationBootstrap.make(errorPresenter:)` factory and a retained Open Object
coordinator. A future wrapper must retain the returned owner and separately
call `configure(_:)` with its application. There is no automatic startup or
`@main` entry point here. The production factory derives its runtime root only
from `Bundle.main.resourceURL/Runtime`; a missing or invalid local resource URL
fails before adapter construction. No environment, current-directory, `PATH`
or selected-user-root fallback is available. The `Runtime` subtree must remain
the existing intact verified runtime; its layout, verifier and lifecycle do not
change.

The production composition resolves the platform-provided `Bundle.main.resourceURL`
through `absoluteURL` before strict coordinator validation. Foundation can return
resource URLs backed by a bundle base; this projection preserves their native
filesystem bytes. Directly supplied resource URLs and document selections retain
the existing strict validation, including refusal of relative or based documents.

The source menu reserves a dedicated application-menu container before the
**File → Open Object…**, Command-O menu; the container adds no commands. Its
asynchronous `NSOpenPanel` chooses files, refuses directories/multiple selection
and disables alias resolution. One picker may be in flight. Cancellation or
no selection performs no open operation; selection and presentation failure
clear picker state. Selected URL arrays are forwarded unchanged through the
same retained `DocumentOpenAdapter` as `application(_:open:)`. The approved
adapter continues to enforce document/cardinality/response and object-reuse
boundaries. This adds no New Object flow, object switching, stop-on-quit,
error-window design or native application/association/installer qualification.

Compile and run only the new injected coordinator proof with an existing
macOS SDK and Swift 6 compiler:

```sh
xcrun swiftc -swift-version 6 -warnings-as-errors \
  native/macos/DocumentOpenAdapter.swift \
  native/macos/ApplicationBootstrap.swift \
  tests/native/macos/ApplicationBootstrapTests.swift \
  -o /absolute/private/temporary/application-bootstrap-tests
/absolute/private/temporary/application-bootstrap-tests
```

This proof instantiates the real coordinator with injected resource URL,
picker, adapter factory, runner, browser and error presenters. A temporary metadata-only
Bundle fixture also exercises the production resource projection and actual
constructors, with no picker/runner/browser calls. The same production menu builder
is also checked as an unassigned `NSMenu` graph: application slot first, File
second, and the exact Open Object action, retained target and Command-O. Tests
verify `NSApp` stays absent. Application assignment and panel composition are
typechecked only: tests never construct `NSApplication` or `NSOpenPanel`,
present UI, launch processes or use a
real workspace/browser/native event/network runtime. The command excludes the
older adapter/helper test driver and does not repeat its 45-second case.
Actual application startup, document events, UI and installed behavior still
require separate exact-artifact acceptance and platform decisions.

## Internal unsigned engineering assembly source

`native/macos/MigrationStreamApplication.swift` supplies an explicit `@main`
entry point. It constructs the approved bootstrap, configures its application,
and retains the bootstrap with `withExtendedLifetime` for the entire
`application.run()` call. Fixed error messages avoid exposing raw controller
output. A bootstrap-construction failure presents an error and exits without
entering `application.run()`. This entry point is typechecked only here; real
application startup, alerts, menus, panels and native events remain unqualified.
It adds no stop-on-quit or object-switch operation.

`scripts/assemble-macos-engineering-app.py` is create-only source tooling for
already-built **trusted** wrapper and runtime artifacts. It does not compile
those artifacts, sign an application, install it or register it with the OS.
The caller must explicitly supply all of:

- Absolute, real non-linked wrapper, runtime and nonexistent destination paths.
- A bundle identifier, bundle version and short version chosen by the operator.
- A numeric minimum OS chosen by the operator.
- `--engineering-policy local-unsigned-engineering`.

There are no metadata defaults, signing/distribution modes or public validator
bypasses. The output is the caller-selected new directory containing
`Contents/MacOS/MigrationStream`, `Contents/Info.plist` and
`Contents/Resources/Runtime`. The runtime subtree retains its bytes and modes;
its manifest and admission rules are not rewritten. Info.plist contains only
basic application identity/version/executable and minimum-OS keys, no document
types, URL schemes, UTIs, entitlements or update configuration.

Before copying, the tool calls the input's fixed
`libexec/migration-stream-local-runtime verify` with an empty environment, no
shell, bounded output and a 120-second deadline with bounded child cleanup.
After copying it verifies the staged runtime through the same fixed entrypoint.
It consumes the actual existing successful CLI envelope and BundleVerification
fields, comparing all reported identity fields except the relocated
`bundleRoot`. It does not invent an inner verification schema or copy an
admission record into the application.

The initial wrapper lane accepts only a thin arm64 Mach-O executable with one
unambiguous macOS deployment command. Missing, wrong-platform, ambiguous,
malformed or oversized deployment evidence refuses real assembly. The explicit
minimum-OS metadata must be at least both the wrapper's parsed deployment
minimum and the runtime report's `target.minimumOsVersion` (currently `11.0`).
This is a **known metadata floor**, not compatibility, linked-library closure,
host qualification or public platform support. The runtime's existing host
policy remains unchanged. A trusted already-built wrapper with deployment
evidence and a matching-host trusted runtime are prerequisites; source
typechecking or a supplied metadata flag cannot substitute for those artifacts.

Publication uses an owned sibling staging directory and Darwin's atomic
`renameatx_np(RENAME_EXCL)`. It never replaces even an empty racing destination;
there is no racy fallback rename in production. Failures clean only owned
staging. A failure after namespace publication can leave the completed output
visible; inspect it rather than assuming a failed command permits replacement.
There is no power-loss/native installed-application qualification claim.

Run the fake-artifact callable self-test without any product verification,
wrapper build, real application, UI, process or OS-registration operation:

```sh
python3 -B scripts/assemble-macos-engineering-app-self-test.py
```

Tests use fake wrapper/runtime bytes, synthetic operator values and internal
validator/inspection injection; process creation is disabled. All fixtures are
ordinary private temporary layout directories, deliberately not `.app` bundles.
The generated fixture plist is ordinary test data. On Darwin the test exercises
actual no-replace publication against an empty racing directory. Elsewhere it
reports that native atomic test as skipped and tests only injected layout flow;
that is not an atomic-publication proof for another host.

The entry point can be checked without building or starting an application:

```sh
xcrun swiftc -swift-version 6 -warnings-as-errors -typecheck \
  native/macos/DocumentOpenAdapter.swift \
  native/macos/ApplicationBootstrap.swift \
  native/macos/MigrationStreamApplication.swift
```

No actual trusted wrapper/runtime assembly or verification was performed by
these source proofs. Existing adapter/bootstrap proofs and the older real
45-second helper case are not rerun by either command. Bundle identifiers,
minimum OS, installer formats, signing, distribution and supported platforms
remain separate operator/owner decisions; this source tooling makes none.
