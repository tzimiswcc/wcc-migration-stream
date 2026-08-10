# Reconciliation policy

This document is the repository-owned source for the immutable version 1
reconciliation policy. The compiled service embeds these exact bytes and binds
their SHA-256 digest into version 2 policy provenance. Editing this document
therefore changes provenance for newly initialized workspaces; it does not
rewrite an existing immutable policy row or activation.

The policy identity remains
`migration-stream/reconciliation-approval-policy/v1`, policy version `1`.
Its authority is limited to local migration-result readiness. It grants no
publication, external target-system acceptance, loading, signing, or production
authority.

The policy treats `ERROR` as the blocking issue severity. The supported rule
kinds are `allowed_values`, `conditional_required`, `max_length`, `pattern`,
`reference_lookup`, `required`, and `unique`; each is blocking, waiver-eligible,
and accepted-as-is-eligible. Exclusion closes a row. A qualifying reevaluation
must be `COMPLETE`, and required integrity state is `VERIFIED`.

`NON_BLOCKING_VALIDATION_WARNING` is the operational warning and requires
acknowledgement. Approval must acknowledge the exact local result-candidate
boundary defined by `migration-stream-result-boundary/v1`. Canonical material
uses `migration-stream-canonical-json/v1` and
`migration-stream-reconciliation-engine/v1`.

On an existing workspace, the service accepts the current neutral provenance
tuple or the single exact legacy tuple identified by its opaque SHA-256
fingerprint. Policy JSON and policy hash must still match version 1 exactly.
Any other provenance, policy, or registration drift is rejected, and an active
policy binding is preserved rather than replaced.
