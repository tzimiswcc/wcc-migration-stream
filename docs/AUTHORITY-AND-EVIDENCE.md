# Authority and evidence

Migration Stream separates technical input, practitioner review, approval, and
runtime authority. That separation makes the local evidence easier to inspect
and prevents an imported file or a convenient browser action from silently
becoming a production claim.

## The local chain

1. A workspace records its identity and operator attribution in the
   startup-selected folder.
2. An offline metadata snapshot or manual definition can create a reviewed
   destination draft. Metadata stays technical evidence.
3. Freezing creates immutable reviewed profile bytes. A separate attributable
   approval of the exact hash is required before mapping/validation authority.
4. An explicitly accepted source package receives bounded structural,
   sample-free profiling evidence.
5. A mapping draft binds the approved profile to that source. Freezing and a
   separate exact-hash approval make it eligible for validation.
6. Validation produces immutable findings. Downstream single-entity governance
   is derived from the active evidence; it does not alter it.

The workbench displays identifiers, hashes, and lifecycle stages where useful.
They are evidence handles, not passwords or a substitute for access control.

## What is intentionally separate

- **Imported metadata** is not business semantics, approval, or external-target
  compatibility.
- **Mapping coverage** is structural and value-free; validation performs the
  data checks.
- **Issue decisions** do not rewrite accepted source data or original
  validation truth.
- **Remediation packs** are local working exports, not Readiness Packages.
- **Readiness Packages** are deterministic local evidence, not delivery,
  transmission, or a target write.

## Data visibility

Broad workbench pages and normal APIs suppress received source values. A
remediation pack contains only explicitly selected key columns, plus an exact
offending value only when separately acknowledged. The values are retained in
the verified local ZIP rather than its broad history view.

## Local boundary

The product is loopback-only and local-first. Loopback is not authentication,
and the current product has no external connector, live target write, or hosted
service boundary. See [Security and data boundary](../SECURITY.md) for the
technical controls and reporting boundary.
