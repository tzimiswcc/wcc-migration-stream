# Route to a public release

The approved public tree for Migration Stream is the clean
`tzimiswcc/wcc-migration-stream` repository produced from the allowlisted
release corpus, not a visibility change on the private development repository.
The development history contains internal evidence, work-package records, and
named handoff material that are intentionally outside the clean tree.

## Ordered milestones

1. **Prove the export and private standalone boundary — complete.** Generate a
   deterministic inventory and source archive from the exact clean commit,
   include only the allowlisted corpus, verify every archived byte, and run the
   documented source gates only from an explicit private standalone destination.
   This closes technical self-containment only. The completed proof created no
   public remote, final public root commit, tag, release, package, binary, or
   visibility change.
2. **Set the legal and ownership terms — complete.** The owner selected the
   Apache License, Version 2.0 for owner-authored material; confirmed copyright
   2026 World Class Consultants G.P.; selected Migration Stream as the product
   name with World Class Consultants G.P. as publisher; approved the Apache-2.0
   trademark boundary; and confirmed the rights to publish all selected
   material. Third-party or adapted text retains its stated terms and
   attribution.
3. **Close source-only supply-chain obligations — complete.** The deterministic
   SBOM captured the reviewed source-only baseline’s 503-component lock universe. A separate review of
   the exact source archive established that it contains no crate or npm package
   payloads, vendored dependency source, generated distribution, or compiled
   dependency executable or library. `NOTICE` identifies the third-party and
   adapted text that is actually present and the attributed SplitMix64
   reference. This conclusion applies only to source-only `v0.1.0`; any wider
   distribution reopens the shipped-closure and licence review.
4. **Establish public governance — complete for the clean public tree.** Add
   an actionable private security-reporting route, support boundary, maintainer
   and review authority, contribution terms, code of conduct, issue forms and
   pull-request guidance. Public issue templates must prohibit client data,
   credentials, workspaces and raw logs.
5. **Dry-run the final source publication before visibility — complete.** The
   owner-approved candidate was imported into a one-commit private clean-history
   mirror, independently cloned, gated, and reviewed with its intended
   repository settings before visibility. The dry run itself did not authorize
   publication; the separate owner authorization completed milestone 6.
6. **Authorize source publication — complete.** The owner approved initial
   public root `1a5b3816c61e74c350ed19826985268dc1b08a22` and the visibility
   transition for `tzimiswcc/wcc-migration-stream`. Source publication does not
   by itself authorize binary distribution or a support claim.
7. **Prepare binary releases separately.** The first public version is
   source-only `v0.1.0` and designates no supported platform. Any later binary
   proposal must separately decide supported platforms, minimum
   operating-system versions, version/tag policy, signing and notarization,
   portable archives versus installers, update/rollback guidance and retention.
   Publish artifacts only after those decisions and their native evidence exist.

## Deterministic export candidate

From a clean Git worktree, generate an inventory outside the repository:

```sh
python3 scripts/public-release-inventory.py \
  --manifest /absolute/private/path/public-release-inventory.json
```

To create and immediately verify the deterministic source candidate as well:

```sh
python3 scripts/public-release-inventory.py \
  --manifest /absolute/private/path/public-release-inventory.json \
  --output-archive /absolute/private/path/public-release-candidate.zip
```

The command reads file bytes from the exact `HEAD` tree, not from mutable
working-tree paths. It refuses a dirty repository, special selected entries,
non-portable or colliding names, destinations inside the private repository,
an existing output destination, or any archive/manifest mismatch. The ZIP uses
fixed metadata, stores each file without compression, and retains Git's
executable-bit authority on every member. It creates no repository, tag,
release, licence or publication claim, and it does not extract the candidate.

## Private standalone verification boundary

A provisional source candidate was materialized in an explicit private local
destination solely to test whether the selected source bytes were
self-contained. That successful result asserts only that the exact candidate
passed the named gates in isolation. The owner has since selected source-only
`v0.1.0` as the first public-release model; that separate decision adds no
binary or platform-support claim to the earlier proof.

The verifier must not create or configure a public remote, push or upload the
standalone candidate, create a tag, release, package, or binary, alter
repository visibility or settings, or vary the committed licence, notice,
contact, maintainer, support boundary, or no-platform-designation decision. Any
disposable local Git history is test state only and must not be promoted as the
final public history.

Standalone verification did not approve the project or repository name,
trademark treatment, copyright wording, rights to publish, dependency licence
conclusions or notices, contribution terms, security-reporting route, support
promise, platform support, source publication, or binary distribution. The
owner's subsequent decisions are recorded separately in the clean public tree.
Source-publication authorization was recorded separately and is complete for
the initial public root. All binary-release questions remain separately gated.

Run its focused regression with:

```sh
python3 -B scripts/public-release-inventory-self-test.py
```

## Locked dependency SBOM evidence

Generate the informational dependency inventory outside the repository from the
same clean exact commit:

```sh
python3 scripts/public-release-inventory.py \
  --dependency-sbom /absolute/private/path/dependencies.cdx.json
```

`--dependency-sbom` can be combined with `--manifest` and `--output-archive`.
All output paths must be absolute, outside the private repository, absent before
the command starts, and distinct even under case-insensitive or Unicode-normalized
filename comparison in the same physical directory.

The output is canonical CycloneDX 1.6 JSON with no timestamp or serial number. It
reads only exact Git blobs from the inventory's source commit: `Cargo.lock`, the
root and workspace `Cargo.toml` files, `rust-toolchain.toml`, `web/package.json`
and `web/package-lock.json`, plus the exact Node pin in `web/.node-version`. Every
authority file and the release-corpus policy must itself be selected by the
public corpus. The command performs no network, package-registry or local package
cache reads and replays clean-worktree and exact `HEAD` checks after capture.

The metadata binds the source commit/tree, validated release-corpus policy hash,
canonical public-inventory hash, individual lock and Node-pin hashes, and one
length-framed aggregate hash over the complete dependency-authority byte set. It
also records the exact Rust and Node pins and verifies that the Node pin satisfies
the identical root-manifest/root-lock engine constraint under the deliberately
small supported grammar: exact `x.y.z`, `^x.y.z`, `>=x.y.z`, and ` || ` joins.

At the current lockfile state, the evidence contains 495 third-party components:
235 checksummed Cargo registry packages and 260 integrity-bound npm package
identities across 262 npm installation entries. The component inventory includes
development, optional, host and platform-specific entries. Identical npm
name/version identities consolidate only when resolved source, SHA-512 integrity,
and unreviewed licence metadata agree. The representative `install-path` remains;
consolidated identities additionally record every sorted occurrence in the JSON
array property `migration-stream:evidence:install-paths`. Metadata reports raw npm
occurrences separately as `migration-stream:evidence:npm-installation-count`.
Direct declarations still resolve through their exact installation paths.

Every local Cargo manifest's declared set must equal its source-less lockfile
package's dependency set. Capture fails closed on unknown source-less Cargo
packages, unsupported Cargo sources, ambiguous direct Cargo references, npm
links, invalid identities, missing or malformed checksums/integrities,
root-manifest or engine drift, direct-pin drift, toolchain or Node-pin drift,
conflicting provenance for one npm identity, or duplicate component
references/package URLs after identical installation consolidation.

Cargo licence data is deliberately not inferred. Each npm `license` value is
preserved verbatim only in a property named
`migration-stream:evidence:unreviewed-npm-lockfile-license-string`; it is not
placed in CycloneDX's legal `licenses` field. Those strings have not been legally
reviewed.

This artifact is a complete **lock-universe component inventory**, not a claim
about the runtime-loaded or distributed dependency set. CycloneDX `dependencies`
relationships are deliberately omitted: transitive dependency requirements are
not semver-validated, and no transitive reachability or relationship graph is
claimed. The artifact is not a vulnerability scan, licence conclusion,
third-party notice, source-publication authorization, binary release
authorization, platform-support statement, or final release SBOM.

## Recorded owner decisions

- Product name: Migration Stream; publisher and copyright holder: World Class
  Consultants G.P.; copyright year: 2026.
- Clean public repository: `tzimiswcc/wcc-migration-stream`.
- Clean root-commit identity:
  `World Class Consultants G.P. <tzimis@worldclassconsultants.gr>`.
- Licence: Apache License, Version 2.0 for owner-authored material, with no
  trademark permission beyond its Section 6 boundary; third-party or adapted
  text retains the terms and attribution stated in its file.
- First release: source-only `v0.1.0`, with no binaries and no designated
  supported platform.
- Initial maintainer, moderator, and release authority: `@tzimiswcc` under an
  explicit single-maintainer model.
- Contributions: bounded pull requests under Apache-2.0 plus Developer
  Certificate of Origin 1.1 sign-off; no CLA.
- Security: private vulnerability reporting with a seven-day best-effort
  acknowledgement target, no remediation SLA, and no bounty promise.
- Support: reproducible defects and bounded proposals through issues, latest
  minor line only, best effort, and no response or resolution SLA.

## Publication record

- Initial public root:
  `1a5b3816c61e74c350ed19826985268dc1b08a22`, with one parentless commit
  and tree `8d7ea7197ed6df6c5c3974fac168393d2143201e`.
- The source archive was independently regenerated byte-for-byte before
  visibility, and the selected source gates passed on that exact root.
- The pre-visibility hosted validation
  [completed successfully](https://github.com/tzimiswcc/wcc-migration-stream/actions/runs/31406186959)
  on the exact root before the repository became public.
- The owner explicitly authorized the visibility transition. The public
  repository now protects `main`, accepts security reports privately, scans for
  secrets with push protection, and keeps binary-release decisions separate.

These gates are complete for the recorded source publication. They do not
authorize a tag, GitHub Release, package, compiled binary, installer, or
platform-support claim.
