# Troubleshooting

Keep the workspace, evidence, and immutable runtime bundle intact while
investigating a problem. Do not delete or edit accepted source packages,
workspace databases, mapping files, historical packages, or runtime evidence to
make a screen look clean.

## The runtime does not start

1. For an assisted bundle, run `verify` first. Stop if it reports a changed,
   missing, linked, extra, wrong-type, wrong-permission, or wrong-platform
   member.
2. Confirm the selected workspace is outside the unpacked bundle and is a
   private local folder.
3. Use `status` with the same workspace and port. It distinguishes an owned
   runtime, stopped/stale state, and a foreign listener.
4. Never use Migration Stream lifecycle commands to replace or kill a foreign
   listener. Choose another unused loopback port if necessary.

## A stage is locked

Read the lock reason in the journey rail and the stage help panel. The rail is
server-derived: changing the address in the browser cannot grant authority.
Typical causes are an unapproved profile, an unapproved mapping, incomplete
coverage, an unfinished validation, or the multi-entity downstream boundary.

## A command looked unsuccessful

Do not immediately repeat a freeze, approval, validation, or package action.
Reload the relevant read view and inspect the authoritative lifecycle state and
hash first. An interrupted browser refresh can occur after a durable local
transaction has completed.

## A revised source cannot reuse the mapping

This is expected when the source structure or relevant authority differs. Prior
evidence remains immutable. Review the source revision/compare evidence and
create a new governed mapping draft when the product reports structural drift.

## Integrity reports a problem

Stop the runtime and run the bundle `integrity` command (or the corresponding
source-build integrity command). Preserve the reported workspace unchanged.
Record the error code, the runtime/bundle identity, and the exact workspace
path privately. Do not attempt manual database repair.

## What to include in a private support request

Include product revision/bundle hash, operating system, the local error code,
stage, and a redacted description of the action. Do not send source extracts,
credentials, client names, connection details, raw logs containing data, or the
workspace database unless a separate secure process is agreed.

There is no public support channel or support-level commitment in the current
scope. See [Release and support status](RELEASE-AND-SUPPORT.md).
