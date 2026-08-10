# Contributing to Migration Stream

Thank you for considering a contribution to Migration Stream, published by
World Class Consultants G.P. Small, focused improvements with clear evidence
are welcome.

Participation is governed by the [Code of Conduct](../CODE_OF_CONDUCT.md).
Security vulnerabilities must follow the private route in
[Security](../SECURITY.md), not a public issue or pull request. GitHub Issues is
not a support channel; see [Support](../SUPPORT.md).

## Before starting

- Search existing issues and pull requests.
- A small, self-contained fix may go directly to a pull request.
- Open a bounded proposal before substantial, cross-cutting, or speculative
  work. Wait for maintainer direction before investing heavily.
- Keep external target claims out of product copy unless separately verified.
- Acknowledgement of an issue or proposal is not a roadmap or acceptance
  promise.

## Security and data boundary

- Preserve the approved canonical profile and mapping as runtime authority.
- Keep fixtures genuinely synthetic or deliberately sanitized.
- Never commit or upload client data, production metadata, credentials, tokens,
  local workspaces, databases, raw logs, screenshots, exports, reports, or
  generated runtime artifacts.
- Do not loosen a check to make a proof pass. Repair the product or update an
  exact deterministic expectation with evidence.
- Use the private vulnerability-reporting route for suspected security issues.

## Contribution terms

Unless a file states otherwise, the project is licensed under the
[Apache License 2.0](../LICENSE). Contributions are accepted under that same
licence: inbound equals outbound. The project does not require copyright
assignment or a Contributor Licence Agreement.

You must have the right to submit every part of your contribution under those
terms. Identify third-party material and its source and licence. You remain
responsible for the provenance, accuracy, and licensing of AI-assisted material
you submit.

Every commit must include a `Signed-off-by` trailer certifying the
[Developer's Certificate of Origin 1.1][dco]. Add it with:

```sh
git commit --signoff
```

The trailer must use a name and email address that identify the contributor:

```text
Signed-off-by: Your Name <your.email@example.com>
```

A DCO sign-off is a certification, not a cryptographic signature. Each person
who contributes content must make the applicable certification. The official
DCO 1.1 text is reproduced verbatim below.

### Developer Certificate of Origin

```text
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.
1 Letterman Drive
Suite D4700
San Francisco, CA, 94129

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## Make a focused change

- Keep the pull request limited to one coherent purpose.
- Add or update tests and documentation where behavior changes.
- Use synthetic fixtures and deterministic assertions.
- Explain consequential authority, security, dependency, or data-boundary
  changes explicitly.
- Do not mix generated build output or unrelated cleanup into the change.

## Local checks

Use focused checks while changing a narrow area, then run the consolidated
checks appropriate to the change:

```sh
bash scripts/ci.sh
cd web
npm ci --no-audit --no-fund
npm run format:check
npm run lint
npm run typecheck
npm test -- --run
npm run build
```

The clean-public-export vocabulary controls are separate:

```sh
python3 scripts/release-corpus-vocabulary-gate.py
python3 scripts/release-corpus-vocabulary-gate-self-test.py
```

Read [Release corpus](RELEASE-CORPUS.md) and
[Release and support status](RELEASE-AND-SUPPORT.md) before making release or
platform claims. A passing check does not create legal, contractual, security,
support, or release authority.

## Pull-request review

Complete the pull-request template, link the relevant issue where one exists,
and report the exact checks you ran. All commits must carry a valid DCO sign-off.
The maintainer may request changes, narrow the scope, or decline a contribution.
See [Governance](../GOVERNANCE.md) for the decision model.

[dco]: https://developercertificate.org/
