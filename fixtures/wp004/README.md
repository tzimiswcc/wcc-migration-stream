# WP-004 synthetic workbench fixture

This directory contains only synthetic, bounded practitioner-workbench evidence.
It reuses the accepted four-row package and reviewed profile from WP-003 and
creates a distinct immutable mapping version for the UI demonstration.

The workbench mapping deliberately removes `PERSON` from the separate
`synthetic-category-domain` validation reference table while preserving the
mapping lookup from source category `1` to canonical `PERSON`. Validation
therefore produces one deterministic `INFORMATION` issue in addition to the
accepted error and warning examples. This is synthetic rule evidence, not a
business-domain decision or a target-compatibility claim.

The incomplete-state demonstration is produced through the normal ingestion
cancellation path in a separate workspace. Browser/UI mocks may also exercise a
bounded `CLEANUP_PENDING` operational warning, but they contain neither a
tombstone path nor tombstone contents and cannot trigger cleanup.

No fixture represents a client, external system, person, tax identifier, credential,
email address, postal address, target payload, or production environment.
