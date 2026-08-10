# Security and data boundary

Migration Stream is a local, practitioner-operated workbench. This document
describes the current product boundary; it is not a security certification,
privacy assessment, hosting guide, or support promise.

## Local service boundary

- The workbench accepts only an explicit loopback listener. Loopback reduces
  exposure but is not authentication and must not be published, proxied, or
  treated as a shared service.
- The product has no external connector, telemetry, hosted deployment, live
  target write, or external-transmission capability.
- The local API contains bounded read routes and bounded same-origin mutation
  routes for the current practitioner journey: workspace establishment,
  destination/profile drafting and approval, source intake, mapping draft and
  approval, validation, source revisions, remediation-pack generation, and
  eligible single-entity downstream local governance. These actions evolve
  local authority only through durable server-side checks; browser navigation or
  caller-supplied labels cannot grant authority.
- Governed mutation requests require the local server's expected same-origin
  protections, intent header, and idempotency handling where the contract
  requires it. `Forwarded` and `X-Forwarded-*` headers are rejected. The API is
  not a proxy-safe or internet-facing interface.

## Workspace and evidence boundary

- A workspace is chosen only at process startup and then established or adopted
  through a local Object Workspace lifecycle. The browser never supplies a
  filesystem path.
- Workspace, descriptor, database, source-package, mapping, profile, and
  publication identities are checked at authority boundaries. Links, reparse
  points, undeclared hard-link aliases, and unexpected identity replacement are
  rejected where supported.
- Accepted source packages, reviewed profile/mapping versions, validation runs,
  revisions, decisions, reconciliation records, remediation packs, and
  Readiness Packages are durable evidence. Do not alter them in place to repair
  a presentation or retry ambiguity.
- A same-user process can still race filesystem namespace operations outside
  the product's cooperative local model. Run the workbench in a private,
  operator-controlled location and rely on operating-system controls as well.

## Data handling

- Repository examples are synthetic or deliberately sanitized. Do not commit
  client extracts, production metadata, credentials, tokens, logs, workspace
  databases, screenshots, exports, or generated reports.
- ZIP content is not extracted. Intake bounds paths, file count, expanded bytes,
  compression ratio, supported compression, duplicate members, encryption,
  links, columns, records, and integrity before use.
- Current source profiling is sample-free. Broad workbench pages and ordinary
  APIs suppress received source values.
- An exact row-detail route is bounded and local. A remediation ZIP contains
  only explicitly selected key columns, plus an offending value only after a
  separate acknowledgement. Its broad history remains value-suppressed.
- Offline metadata parsing rejects DTDs and never resolves external entities.
  Imported metadata is technical evidence, not a compatibility or authority
  claim about another system.

## Logs, diagnostics, and packages

- Normal API errors and startup diagnostics use bounded messages and avoid
  source values and local paths where practical.
- Runtime session controls and logs are private local diagnostics, not governed
  migration evidence. Keep them out of Git and do not transmit them casually.
- Local Readiness Packages and remediation packs are data-bearing local files.
  Their creation does not transmit data, deploy a service, or write to an
  external target.
- Run integrity against a stopped runtime when the assisted launcher requires
  it. Preserve a failing workspace unchanged and record the error code and
  runtime identity privately rather than attempting manual database repair.

## Supported versions

The initial public milestone is source-only `v0.1.0`; no public binaries or
platform-support commitment is included. Before a public release exists, no
public version is supported. Once releases exist, security maintenance focuses
on the latest published minor line and latest patch within it, best effort.
Superseded minor lines do not have a promised security-fix or backport service.

## Report a vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's private
vulnerability-reporting form for the public repository:

[Privately report a vulnerability](https://github.com/tzimiswcc/wcc-migration-stream/security/advisories/new)

`@tzimiswcc` monitors repository security advisories. The project aims to
acknowledge a report within seven calendar days, best effort. This is a target,
not a service-level agreement or guaranteed response time. The project offers
no guaranteed fix or disclosure date, bounty, or paid reward.

Include a concise description, the affected version or commit, potential
impact, and minimal reproduction steps using only synthetic or deliberately
sanitized material. Do not send client data, production metadata, credentials,
tokens, workspace files or databases, raw logs, screenshots, exports, generated
reports, or other confidential material. If a safe report cannot be prepared
without such material, do not upload it; first create a sanitized reproduction.

Security advisories are for vulnerabilities, not ordinary bugs or support
questions. Use the repository's bug form for non-sensitive defects.

## Limits

The Apache License 2.0 does not resolve data protection, confidentiality,
client-contract, or secure-operating obligations. Do not use current loopback
controls as a substitute for access control, network isolation, endpoint
protection, backups, or an approved data-handling process.

See [Support](SUPPORT.md) for the public maintenance boundary and
[Governance](GOVERNANCE.md) for the initial security authority.
