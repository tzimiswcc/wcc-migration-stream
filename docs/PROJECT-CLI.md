# Trusted project CLI

The `migration-stream project` group wraps the accepted project and workspace
APIs. It operates on a path selected by the trusted local caller. It adds no
browser filesystem API and starts no runtime server.

For an ordinary requested **new empty object**, a future native caller can use
one operation:

```sh
migration-stream project create \
  --directory /absolute/new-object-directory \
  --display-name "Customer readiness" \
  --initialize-and-bind \
  --migration-object-name-or-scope "Customer readiness" \
  --creator-attribution "Local operator"
```

`--initialize-and-bind` is the explicit new-object initialization intent. The
scope and creator fields are the existing workspace identity/attribution
contract, supplied by the caller; there is no new approval or acknowledgement
form. The command creates a previously absent object directory, initializes
that freshly retained empty workspace through the existing command/CAS, and
explicitly binds the project. Workspace UUID and authoritative descriptor are
created only by `ObjectWorkspaceService`; the CLI never invents or rewrites
them. The response's `descriptorPath` names the final `project.mstream`.

This command refuses every existing target directory, including an empty
folder, a completed object or a previous partial attempt. It never adopts,
repairs, deletes, overwrites or automatically resumes existing material.

For deliberate staged setup, omit the initialization option:

```sh
migration-stream project create --directory /absolute/new-object-directory \
  --display-name "Customer readiness"
migration-stream project inspect \
  --project /absolute/new-object-directory/project.pending.mstream
```

The initial response requires `INITIALIZE_WORKSPACE_EXPLICITLY`, and no SQLite
or workspace identity is established by default. Complete the existing explicit
workspace initialization workflow, then bind its active identity:

```sh
migration-stream object-workspace initialize \
  --workspace /absolute/new-object-directory/workspace \
  --display-name "Customer readiness" \
  --migration-object-name-or-scope "Customer readiness" \
  --creator-attribution "Local operator"
migration-stream project bind \
  --project /absolute/new-object-directory/project.pending.mstream
migration-stream project inspect \
  --project /absolute/new-object-directory/project.mstream
```

Inspect is read-only: it does not establish authority or silently bind an active
workspace. Its versioned project view reports descriptor, canonical paths,
existing lifecycle and the required next action. Binding requires the active
existing workspace and uses its writer lock; it never initializes or adopts.
After binding, use the returned final descriptor path. A stale pending receipt
is not an alternative way to open the final project.

A failure retains the existing stable CLI `ok/code/message` contract. Project
creation/binding failures additionally report `project.phase`, selected directory,
a known retained descriptor path where available, and `nextAction`. A failed
initialization or binding leaves partial material intact for explicit inspection
and the existing initialization/recovery workflow. Retrying `create` refuses the
existing directory. A publication error can leave a final descriptor visible;
inspect the indicated pending/final state before choosing an explicit next step.
Missing required initialization arguments fail during parsing before creation.

Placing files under `input/` grants no accepted source/profile authority. These
commands do not ingest, import, approve a profile, adopt or relocate evidence,
export data, launch a browser or create an installer. To launch a bound object,
use the separately reviewed verified-bundle controller:

```sh
./migration-stream-local start --project /absolute/new-object-directory/project.mstream --port 43210
```

`tests/project_cli.rs` runs real CLI processes over synthetic temporary objects:
pending/create/inspect/bind, explicit one-operation initialization, no-clobber,
partial failure, invalid descriptors/identity, read-only inspect, writer-held
binding rejection and zero intake/profile-approval side effects. Existing CLI
regressions remain separate from the previous runtime and lifecycle proofs.
Previously reviewed immutable bundles are not rebuilt or promoted by this slice.

JSON responses require representable UTF-8 paths. Failure-context display paths
are safely encoded and marked `pathsAreLossy` when necessary; callers must keep
the original native path rather than treating a lossy display string as an exact
new selection. A response-serialization failure retains partial material and
returns a controlled error; it does not silently rename or repair the object.
