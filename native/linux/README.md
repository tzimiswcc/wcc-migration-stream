# Linux document-open launcher source

This directory prepares a Linux desktop document-open adapter and uninstalled
source templates. It adds no installer, registered association, package,
architecture, distribution, Python-version guarantee or supported-platform claim.
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
