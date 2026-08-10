# Governance

Migration Stream is licensed under Apache License 2.0 by World Class Consultants
G.P. This document records the governance model for the source project. It does
not create a support, roadmap, release, or service commitment.

## Initial maintainer model

`@tzimiswcc` is the sole initial maintainer, Code Owner, Community Moderator,
security responder, and release authority. The project intentionally starts
with a single-maintainer model. Contribution does not by itself grant project
authority, maintainer status, ownership, or a vote.

The maintainer has final responsibility for:

- product scope, architecture, priorities, and roadmap choices;
- accepting, requesting changes to, or declining issues and pull requests;
- merging changes and administering the repository;
- vulnerability handling and coordinated disclosure decisions;
- Code of Conduct enforcement; and
- versioning, tags, releases, and any future distribution decision.

Feedback and reasoned disagreement are welcome. The project does not use voting
and does not promise to implement an accepted suggestion or publish a roadmap.

## Changes and review

Changes should arrive through focused pull requests. External pull requests
require explicit maintainer acceptance. Owner-authored work is not subject to a
fictional self-approval requirement, but it remains subject to the applicable
automated checks and recorded review of consequential changes.

The maintainer may close work that is out of scope, too broad to review safely,
insufficiently evidenced, duplicative, or inconsistent with the project's
security and data boundaries. Substantial work should be discussed in an issue
before implementation. See [Contributing](docs/CONTRIBUTING.md).

## Appointments and succession

The maintainer may appoint additional maintainers and define or remove their
responsibilities. World Class Consultants G.P. may designate a successor if the
initial maintainer can no longer serve. Any change to the public maintainer or
moderation model should be recorded in this file and in `CODEOWNERS` before the
new authority is relied upon.

If a second active maintainer is appointed, the project should reassess review,
security, and conduct-reporting separation. Until then, the single-moderator
limitation is stated explicitly in the [Code of Conduct](CODE_OF_CONDUCT.md).

## Releases

Only the release authority may create an official Migration Stream release.
The initial publication milestone is source-only `v0.1.0`. It does not authorize
binaries, installers, signing claims, or platform-support promises. Those
require a separate recorded decision and appropriate evidence.

See [Support](SUPPORT.md) and [Security](SECURITY.md) for the current maintenance
and reporting boundaries.
