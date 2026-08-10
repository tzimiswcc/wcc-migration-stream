# Operator and Release Identity Checklist

Use this checklist for a separately authorized synthetic operator proof or
release-evidence run. It controls identity and repeatability; it does not grant
authority to use real client data, generate a Readiness Package, contact an
external target system, deploy, publish externally, or approve production use.

## 1. Source identity

- [ ] Record the branch and exact revision.
- [ ] Confirm the worktree is clean, or record every changed, staged, and
      untracked file explicitly.
- [ ] Record the local/cached-origin ahead-behind relationship when it matters.
- [ ] Stop if the source state differs from the authorized scenario.

## 2. Build identity

- [ ] Record the Rust, Cargo, Node.js, and package-manager versions.
- [ ] Use locked commands where supported.
- [ ] Name the exact executable target; do not assume a default build rebuilt
      the workbench binary.
- [ ] Produce the exact frontend production build intended for serving.

## 3. Executable inventory

- [ ] Record the executable's absolute path.
- [ ] Record SHA-256, byte size, and modification time.
- [ ] Record inode or the platform-equivalent file identity when meaningful.
- [ ] Confirm the inventory was captured after the last relevant source change.

## 4. Frontend asset inventory

- [ ] Inventory every production asset that the workbench will actually serve.
- [ ] Record each exact filename, SHA-256, and byte size.
- [ ] Record which executable/process and asset directory form the intended
      runtime pair.
- [ ] Stop if an asset cannot be matched to the intended frontend build.

## 5. Pre-launch verification

- [ ] Reconfirm source, executable, and asset identities after the final change.
- [ ] Use the absolute executable and asset paths.
- [ ] Record the exact loopback listener and every capability/mode flag.
- [ ] Use only the separately authorized synthetic workspace.
- [ ] Confirm accepted and historical workspaces, packages, databases, reports,
      journals, screenshots, and other runtime evidence will remain immutable.

## 6. Running-process verification

- [ ] Record process identity and the actual executable path.
- [ ] Where the platform permits, match the running executable to the recorded
      path and SHA-256.
- [ ] Confirm the listener is the expected local loopback endpoint.
- [ ] Inspect the advertised context contract and capability set.
- [ ] Stop if any unexpected contract, capability, listener, or external-write
      path appears.

## 7. Frontend/runtime correspondence

- [ ] Confirm the browser receives the intended production asset filenames.
- [ ] Inspect `context/v4`, or the current implemented context contract.
- [ ] Confirm only the expected local capabilities are exposed.
- [ ] Do not infer an external-write capability from local package generation,
      a technical `Publication` record, or a browser label.

## 8. Safe command discipline

- [ ] Inspect authoritative server state before retrying.
- [ ] For non-idempotent commands, submit once and read authoritative state
      before any retry.
- [ ] Do not infer failure merely from delayed or stale browser feedback.
- [ ] Distinguish server evidence from transient browser presentation.
- [ ] Stop rather than retry when command identity or outcome is ambiguous.

## 9. Restart and readback identity

- [ ] Stop and restart deliberately.
- [ ] Reconfirm the executable and frontend asset identities after restart.
- [ ] Verify persisted history, detail, or download readback only when the
      scenario separately authorizes it.
- [ ] Do not create a duplicate package merely to prove persistence.

## 10. Evidence handling

- [ ] Use synthetic data only unless a separate authority explicitly says
      otherwise.
- [ ] Preserve historical workspaces and packages byte-for-byte.
- [ ] Never repair failed or superseded evidence in place.
- [ ] Label superseded and accepted evidence unambiguously.
- [ ] Keep runtime evidence out of Git unless an explicit evidence policy
      requires otherwise.

## 11. CI economy

- [ ] Use focused local checks during development.
- [ ] Run one consolidated broad local validation after stabilization.
- [ ] If separately authorized, use one routine hosted PR validation against
      the exact intended commit.
- [ ] Avoid repeated polling, cosmetic reruns, and unchanged retries.
- [ ] Do not run a heavyweight native matrix unless cross-platform release
      evidence is explicitly required.

## 12. Stop conditions

Stop immediately if:

- [ ] source changed after the recorded build;
- [ ] executable identity is uncertain;
- [ ] served assets cannot be matched to the intended build;
- [ ] an unexpected contract or capability appears;
- [ ] real or client data is encountered;
- [ ] external transmission or target-system interaction would occur;
- [ ] accepted evidence would need alteration; or
- [ ] authority, approval, command outcome, or production status is ambiguous.
