# Clean public export corpus

`config/release-corpus.json` defines the product source set used to construct
the clean repository and future exact source candidates. It is not a dependency
or binary-distribution manifest. The vocabulary gate checks every selected path
and text file, using deterministic paths and case-insensitive matching for
prohibited vendor identities and legacy technical identifiers.

Historical work-package records, completion reports, architecture decisions,
showcase material, benchmark evidence, and dedicated named handoff material are
excluded. They are retained as internal history and are not rewritten or
represented as public product material. The selected source documentation is
the concise README plus the named practitioner and release guides. This scope
is intentionally narrower than the private development repository.

The selected corpus includes the Apache-2.0 licence and notice, public
security, conduct, contribution, governance, and support policies, and the
bounded GitHub issue, pull-request, and ownership metadata that implements
those policies. It also includes the Git ignore/attribute authority needed for
clean cross-platform checkouts, the benchmark definitions consumed by selected
tests, and one read-only Ubuntu source-validation workflow. Historical benchmark
evidence, pre-publication assisted-runtime installation and quickstart guides,
the private routine CI workflow, and private native-evidence workflows remain
excluded. `.github/workflows/public-source-ci.yml` is the only selected workflow.
This selection establishes the source-publication boundary; its evidence does
not by itself authorize a visibility change or binary release.

For the source-only `v0.1.0` candidate, `NOTICE` records the third-party and
adapted text actually present in the selected corpus and the attributed
SplitMix64 reference. The exact source archive contains dependency lockfiles as
metadata but no crate or npm package payloads, `node_modules`, vendored
dependency source, generated distribution, compiled dependency objects, or
compiled dependency executables or libraries. The 503-component deterministic
SBOM is therefore lock-universe evidence, not a statement that those package
payloads are shipped. Adding any vendored, generated, packaged, runtime, or
binary material requires a new closure and licence-obligation review.

In the private development repository, routine pull requests limited to
documentation, the exact public community metadata, or the corpus policy and
its gates use the short documentation lane. New or changed workflows, unknown
`.github` paths, and product code continue to use the full Linux lane. The
classifier workflow itself is private and excluded from the public corpus;
changing it deliberately selects the full Linux lane.
Native Windows evidence remains governed separately. Changes limited to the
governance, community, and corpus-policy paths listed above do not trigger that
matrix; its small explicit set of runtime-facing documentation paths still
does.

Run the gate with:

```sh
python3 scripts/release-corpus-vocabulary-gate.py
python3 scripts/release-corpus-vocabulary-gate-self-test.py
python3 -B scripts/public-release-inventory-self-test.py
```

The assisted-runtime bundle runs the same gate before it records its source
inventory. A passing gate establishes only vendor-vocabulary hygiene for this
scoped corpus. It does not establish external-target compatibility, connection,
write, delivery, legal clearance, or publication authority.

The separate [public-release route](PUBLIC-RELEASE-ROUTE.md) describes how to
inventory and archive a non-publishing clean-export candidate from an exact
clean commit. That evidence does not by itself authorize a tag, GitHub Release,
package, binary, or visibility change.
