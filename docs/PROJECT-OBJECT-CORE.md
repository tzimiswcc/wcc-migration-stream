# Project object library slice

`project_object::{create_project, open_project, bind_project}` wraps the existing
workspace service. It does not change the workbench startup interface or launch a
runtime. A trusted future launcher must still use the existing runtime lifecycle,
listener checks and workspace writer lock.

Creation takes a new object directory path under an existing parent and a display
name. It refuses an existing directory, including an incomplete previous attempt.
It creates `workspace/`, `input/`, `output/` and `project.pending.mstream`. Opening
this receipt inspects the current workspace lifecycle without initializing it.
Inputs are ordinary unaccepted files; this API does not import them.

The caller presents the existing explicit workspace initialization workflow using
`ObjectWorkspaceService`. The library never constructs that command, assigns its
workspace identity or rewrites `workspace.json`. After initialization, an explicit
`bind_project(pending_path)` acquires the existing workspace lock, validates active
authority and durable integrity, then publishes `project.mstream` without replacing
any file. The final small JSON descriptor records schema `migration-stream/project/v1`,
envelope UUID, display name, the literal relative workspace reference `workspace`,
and the expected authoritative workspace UUID and descriptor hash.

The pending receipt and final descriptor are separate immutable publications,
not an in-place rewrite. The receipt is retained. If final publication succeeds
but its response is interrupted, reopen `project.mstream`; reopening the pending
receipt refuses when the final name exists. If interrupted before publication,
reopen the pending receipt and explicitly bind after initialization. Existing
workspace initialization already has its own exact pending-command recovery.
Incomplete directory creation without a receipt fails closed and needs explicit
inspection; the API does not delete, repair or overwrite that directory.

Bound opens validate the descriptor version/fields, constrained reference,
non-link layout, exact active identity/hash and existing integrity replay.
Opening is read-only and does not acquire a writer reservation; an object may be
inspected while its runtime is open, but starting a runtime still follows existing
single-writer and foreign-listener rules. Explicit binding fails while another
workspace writer holds its lock. Existing legacy workspaces need a separately
reviewed adoption interface; this create-new API does not provide one.

This is source/API work with synthetic native tests only. Installers, native file
associations, runtime launching, relocation, backup/restore and Windows/Linux
qualification remain outside this slice. Filesystem checks retain the project
root and the existing service retains workspace identity. They are not a promise
of protection against every malicious concurrent mutation by a user who can
rewrite the entire project tree.

New-root creation synchronizes the existing parent's directory entry on Unix
before creating the child layout. A parent-sync error is returned with its path;
the partial root remains and a create retry refuses to overwrite it. This reuses
the existing Unix directory-sync mechanism. The existing portability layer has
no equivalent directory fsync on Windows or other non-Unix targets; successful
creation there does not establish parent-entry crash durability. Native power-loss
qualification remains separate.
