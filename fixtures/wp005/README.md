# WP-005 synthetic governed-decision proof pack

This pack contains synthetic inputs used by the selected governed-decision test
targets. Generated workspaces, databases, WAL/SHM files, packages, browser
caches, and measurement outputs are not part of the pack.

The executable proof cases live in `tests/wp005_decisions.rs`,
`tests/wp005_schema_migration.rs`, and `tests/wp005_workbench.rs`. Together they
cover the four closed dispositions, exact/empty/null overlays, resolved,
residual, and newly created validation issues, supersession, stale concurrency,
idempotent replay and conflicting reuse, competing writers, bounded text,
schema migration, source/original-projection immutability, hard-abort recovery,
capacity rejection, structurally rehashed tamper, tombstone exclusion, transport
protections, and read-only/governed capability separation.

Run the selected proof from the repository root:

```sh
cargo test --locked --test wp005_decisions --test wp005_schema_migration --test wp005_workbench
```

No fixture represents a real person, customer, company, tax identifier, email,
address, external source/target, client system, credential, production environment,
target payload, migration-result approval, or publication authority.
