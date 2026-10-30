# Linux document-open launcher source

This directory prepares a Linux desktop document-open adapter, uninstalled
source templates and the source packaging route below. No generated package or
registered association is included, and no supported-platform claim is added.
See [Release and support status](../../docs/RELEASE-AND-SUPPORT.md).

`DocumentOpenLauncher` requires a trusted intact runtime root and an explicit
browser callback. It invokes only that root's fixed
`libexec/migration-stream-local-runtime`, with byte argv `open`, `--`, and exactly
one absolute document path. Python's filesystem encoding and surrogateescape
preserve native Linux byte paths; the adapter neither resolves nor rewrites the
document. Relative names and URL strings are refused. The existing controller
validates the bound descriptor, verifies its bundle, and owns runtime selection,
startup and cross-process ownership. This source adds no Start implementation.

The retained adapter refuses overlapping calls and, after validated success,
refuses a different document even if presentation failed. Separate desktop
invocations rely on the existing controller's cross-process guards. Only a
successful exact CLI/status schema, Boolean `ok: true`, `OWNED`, literal
`127.0.0.1:4173` and exact `http://127.0.0.1:4173` allow presentation. Duplicate
JSON keys, malformed output and overlong streams are refused. The callback
receives the fixed literal URL; response fields, paths, child diagnostics and
capabilities are never forwarded to it or printed on failure.

The production runner uses bytes argv without a shell, an empty controller
environment and closed inherited descriptors. Each captured stream is capped
at 64 KiB. It waits at most 45 seconds, then terminates only its direct child,
with two seconds before kill and two seconds for reap. A pipe inherited by a
descendant cannot force an EOF wait. An operating-system failure to reap can
still surface as an exception. No session/runtime PID is signaled.

## Explicit operator composition

`launch(arguments, trusted_runtime_root=..., browser=...)` is the real adapter
entry point. All installation authority must come from trusted operator
composition. A missing presenter fails before a controller call.
`launcher_bootstrap.py.in` illustrates that composition; it is deliberately an
uninstalled substitution template, not a configured executable. Required bindings:

| Binding | Trusted operator decision |
| --- | --- |
| `@ABSOLUTE_PYTHON@` | Existing, reviewed Python interpreter |
| `@ABSOLUTE_BOOTSTRAP@` | Final substituted bootstrap source location |
| `@ABSOLUTE_ADAPTER_DIRECTORY@` | Directory containing this reviewed adapter |
| `@ABSOLUTE_RUNTIME_ROOT@` | Intact matching runtime bundle root |
| `@ABSOLUTE_PRESENTER@` | Reviewed executable accepting a URL as its final argument |

This minimal template restricts every substitution to an absolute ASCII path
matching `/[A-Za-z0-9_./-]+`. No spaces, percent signs, quotes, equals signs,
backslashes, shell characters or newlines may be inserted. This is a restriction
of the example installation bindings, not of document argv. A future installer
supporting broader installation paths must implement and test the specification's
escaping rules instead of interpolating untrusted strings. The source includes
no substitution or installation command.

The operator must review any fixed presenter arguments and explicitly provide
only its required desktop-session environment in `PRESENTER_ENVIRONMENT`.
Nothing discovers a browser or executable through PATH, environment variables,
project contents or current directory. The example presenter uses a ten-second
bounded invocation and the same stream caps; long-running browser executables
may require a different reviewed desktop presenter callback. No presenter is
assumed to be installed or compatible. The desktop entry selects Python with
`-I` so Python environment variables and the current directory cannot supply
imports; the bootstrap explicitly adds only the trusted adapter directory.

`MigrationStream.desktop.in` uses the freedesktop `%f` single-filename field,
not a URL field. `DBusActivatable=false` keeps activation on that Exec path.
No MIME type is assigned; document MIME identity, association registration and
handling of remote-origin files copied locally by a desktop remain separate
operator decisions. The adapter cannot infer a file's provenance from argv.
The template is hidden from menus and emits a generic stderr failure; native
error dialogs and desktop integration remain future work.

Reference: [freedesktop Desktop Entry Specification, Exec key](https://specifications.freedesktop.org/desktop-entry/latest-single/#exec-variables).

## Source proof

With an existing Python interpreter, from the repository root:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s native/linux -v
```

Tests use injected runners/presenters, temporary synthetic documents, and the
existing Python interpreter running tiny synthetic helper code. They cover
native argv, strict response refusal, pinning, busy/error recovery, both pipe
caps, direct-child timeout cleanup and inherited-pipe exit. They start no product
controller, runtime, browser or desktop integration. Passing on another POSIX
host proves these source behaviors only; Linux desktop events, selected
interpreter/presenter, exact bundled artifact and native lifecycle require
separate acceptance on the chosen Linux environment.

## Ubuntu and portable packaging source

`scripts/package-linux-desktop.py` creates a `.deb` and a portable `.tar.gz`
from one already-built, trusted, matching native GNU/Linux Runtime. The exact
matrix lanes are `x64` / `x86_64-unknown-linux-gnu` (Debian `amd64`) and
`arm64` / `aarch64-unknown-linux-gnu` (Debian `arm64`). Run the packager with
Python 3.11 or later on the matching native Ubuntu architecture. It never builds
the product, installs a package, launches a workbench/browser, starts a service
or promotes the matrix's verification status.

```sh
python3 -I -B scripts/package-linux-desktop.py \
  --runtime "$REVIEWED_RUNTIME" \
  --destination "$NEW_OUTPUT_DIRECTORY" \
  --architecture x64 \
  --expected-product-commit "$REVIEWED_PRODUCT_COMMIT" \
  --expected-source-snapshot-sha256 "$REVIEWED_SOURCE_SNAPSHOT" \
  --expected-manifest-sha256 "$REVIEWED_MANIFEST_SELF_HASH" \
  --maintainer "$APPROVED_DEBIAN_MAINTAINER" \
  --revision 1 \
  --engineering-policy local-unsigned-engineering
```

Use `--architecture arm64` for an ARM64 native build. Every path must be
absolute, have real existing ancestors, and contain no parent traversal.
The output directory must be absent and outside Runtime. Existing outputs and
collisions are refused. Linux `renameat2(RENAME_NOREPLACE)` publishes the final
directory without replacement; there is no weaker rename fallback.

The package name and upstream version come from this checkout's `Cargo.toml`
and must match the production verifier's `productVersion`. The Debian version
adds the verified clean commit's first twelve digits and the explicit revision.
The Maintainer field is an explicit reviewed name/email binding; the source
does not invent a permanent publisher identity or signing certificate.
The fixed controller's real `verify` runs before and after copying. Its exact
target, clean commit, source snapshot, manifest self-hash and native executable
hashes must match the captured bytes and caller's admitted provenance pins.
These are byte/source checks, not publisher authentication.

On the native Ubuntu build host, the fixed `/usr/bin/dpkg-shlibdeps` derives
shared-library dependencies for the three existing native Runtime executables.
It must be present (the Ubuntu `dpkg-dev` build tool) and succeed without ignored
missing libraries/symbol information. No dependencies are downloaded or
installed by this script. Its actual dependency result and Ubuntu series are
recorded, with `python3 (>= 3.11)`, `xdg-utils`, `shared-mime-info` and
`desktop-file-utils` added for the explicit desktop composition. No guessed
glibc floor or compatibility with another distribution is claimed. The portable
archive retains the same native library requirements; it is not self-contained.

The `.deb` installs the unchanged Runtime under
`/usr/lib/migration-stream/Runtime`, with adapter/launcher sources beside it.
The desktop and shared MIME source files go under `/usr/share/applications`
and `/usr/share/mime/packages`. They add an Open-with handler for the local
`application/x-migration-stream` / `*.mstream` association and never make it the
default. The desktop entry keeps `NoDisplay=true`: this adapter takes exactly
one object and has no zero-argument Start-menu implementation. Native package
installation and distribution cache-trigger behavior require separate Ubuntu
qualification. No maintainer script or autostart/service file is included.

The portable archive includes `packaged_launcher.py`, the existing adapter,
Runtime, notices and instructions. Open it with the existing isolated
interpreter, passing one absolute object path:

```sh
/usr/bin/python3 -I /absolute/layout/packaged_launcher.py /absolute/object.mstream
```

Optional user-only registration is a separate explicit operation:

```sh
/usr/bin/python3 -I /absolute/layout/packaged_launcher.py \
  --register-desktop /absolute/existing-user-owned-data-home
```

Registration requires a non-root user, an existing user-owned data home, and
the existing template's ordinary absolute ASCII installation binding
`/[A-Za-z0-9_./-]+` (no parent traversal). This restriction applies to the
installation binding, not object filenames. It creates only the two named
association source files, refuses either existing file before writing, and
runs the fixed MIME/desktop user-cache update tools with bounded output/time.
It never modifies `mimeapps.list`, registers a default, installs the Runtime or
deletes shared directories. A failed update retains created files for review.

The presenter is explicitly `/usr/bin/xdg-open`, receiving only the existing
validated constant loopback URL. Its environment contains a fixed system PATH
and a named allowlist of desktop-session values; execution overrides such as
`BROWSER`, `LD_PRELOAD` and Python variables are excluded. The controller still
receives the existing empty environment. Consequently its verified Linux
fallback state is `/tmp/migration-stream-runtime-<uid>/session-<key>`, outside
the immutable Runtime and selected workspace. No global application state is
installed. Interpreter, desktop presenter/session and installed lifecycle still
need native qualification.

Input and payload captures refuse links, hardlinks, special entries, changes
of bytes/native identity, more than 4096 graph entries, depth over 64, files
over 128 MiB and total captured bytes over 513 MiB. Archive writes are capped
at 1 GiB, each tool stream at 64 KiB, each verifier at 120 seconds, dependency
analysis at 60 seconds and the CLI operation at 360 seconds. Modes and bytes
inside Runtime are preserved; all tar members are explicit regular files or
directories, never hardlink entries.
Both package formats use explicit POSIX USTAR headers, with no PAX extensions.
Names or metadata that USTAR cannot represent refuse instead of changing
formats; this is stricter than the Runtime's own maximum path length.
The bounded receipt outside Runtime records
its graph, production verifier identity and both artifact digests. Private
working directories are retained beside the output even on success; a refusal
may also retain its private unpublished result directory. There is no automatic
recursive cleanup of uncertain paths, retry, overwrite or verification bypass.

Focused source proof, using only synthetic ELF/Runtime bytes and injected
verifier/dependency/publication/cache callbacks:

```sh
python3 -I -B scripts/package-linux-desktop-self-test.py
```

This checks real ar/tar format bytes, both target mappings, Runtime modes/graph,
refusals and desktop composition. It invokes no native product, package tool,
browser, installer, real cache update or device, and does not qualify Ubuntu,
Linux ARM64 or a portable distribution.

Packaging references: [Debian binary format](https://manpages.debian.org/trixie/dpkg-dev/deb.5.en.html),
[native dependency analysis](https://manpages.debian.org/trixie/dpkg-dev/dpkg-shlibdeps.1.en.html),
[desktop single-file activation](https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html),
and [shared MIME registration](https://specifications.freedesktop.org/shared-mime-info/latest/ar01s02.html).
