# Apple-silicon engineering application and disk image

`scripts/package-macos-engineering-desktop.py` prepares a local, unsigned
engineering package from an already-built trusted Swift wrapper and intact
native Runtime. It produces a new directory containing exactly:

- `Migration Stream.app`
- `Migration Stream.dmg`
- `engineering-package.json`

This is free local engineering tooling, not a signed/notarized distribution or
a product installer. No Apple account, purchase, keychain identity, certificate,
notarization service, installation, trust-store change or security bypass is
selected. Existing input signatures, including linker-generated ad-hoc
signatures if present, are preserved rather than inspected or replaced.
The package may encounter Gatekeeper restrictions; it does not claim public
platform support, application/UI acceptance or an update channel.

## Inputs and create-only behavior

The caller must supply absolute real, nonlinked input paths and an absent output
directory, plus the same explicit identity/version/minimum-OS values required by
the existing [engineering assembler](ASSISTED-LOCAL-RUNTIME.md#internal-unsigned-engineering-assembly-source):

```sh
python3 -B scripts/package-macos-engineering-desktop.py \
  --wrapper /absolute/private/trusted-wrapper \
  --runtime /absolute/private/intact-runtime \
  --output-directory /absolute/private/new-engineering-package \
  --bundle-identifier local.migrationstream.engineering \
  --minimum-os MAJOR.MINOR.PATCH \
  --bundle-version 1 \
  --short-version 0.1.0 \
  --engineering-policy local-unsigned-engineering
```

Replace the minimum-OS placeholder with an explicit value at least as high as
both the wrapper's Mach-O deployment command and verified Runtime minimum.
Metadata floors do not prove host compatibility. Wrapper/compiler provenance
remains an external operator prerequisite: the wrapper digest identifies bytes,
not its source or publisher. Build the wrapper and Runtime from the selected
same-source engineering checkout and retain that external build receipt.

The assembler's existing thin ARM64 Mach-O, deployment, real-file/directory,
no-link, bounded graph, byte/hash/mode and Runtime report checks remain intact.
The package requires a CLEAN Runtime. It preserves `Contents/Resources/Runtime`
without rewriting its manifest or compiled identity. The existing Swift app
entry, retained bootstrap, Open Object coordinator and adapter are reused; this
adds no document UTI, automatic association, application menu or lifecycle code.

A private sibling stage receives the application. The package calls the fixed
trusted Runtime controller's read-only `verify` through the existing bounded
assembler helper: source, copied application, prepublication and final output
verification. It compares all verification identity fields except relocated
`bundleRoot`, and replays source identities plus application bytes/modes.
Those four verification calls start product processes/threads; they select no
Start/Open/project/server/browser action. No such process is run by the
synthetic source tests.

The new whole-application snapshot conservatively retains the existing
assembler's 4096-entry, depth-64 and 513 MiB aggregate bound; individual members
remain bounded to 128 MiB. A large Runtime that fits its own bounds can still
exceed this stricter application-plus-wrapper packaging bound and refuse.
The image is limited to 1 GiB and receipt to 1 MiB. These are observed file/input
bounds, not filesystem, CPU or memory quotas.

Publication uses the existing Darwin `renameatx_np(RENAME_EXCL)` sibling-directory
publisher. Existing destinations and output races refuse without replacement.
Failed private stages are deliberately retained, including when an image helper
or publication result is uncertain. A post-publication failure preserves the
visible package for inspection; it never deletes it or reports success.

## Disk image contract and proof limits

The production image path uses the existing fixed `/usr/bin/hdiutil` only:

```text
create -quiet -fs HFS+ -format UDZO -volname "Migration Stream" -srcfolder OWNED_IMAGE_ROOT OWNED_IMAGE.dmg
verify -quiet OWNED_IMAGE.dmg
```

No shell, inherited environment, standard input, output overwrite, encryption,
certificate, `attach`, `open`, Applications alias or installation operation is
requested. Each direct image child has a 300-second deadline and separate
64 KiB output/error caps. Cleanup addresses only the retained direct child,
with bounded terminate/kill/wait; it does not kill unrelated processes or claim
control of image-tool descendants. OS calls are not real-time interruptions.
Image creation may use internal Apple disk-image/filesystem helpers; this source
proof has not observed or qualified their actual effects.

Apple's installed `hdiutil(1)` manual states that `create -srcfolder` copies files
into a fresh filesystem, and `verify` checks stored read-only/compressed image
blocks, **not** filesystem consistency. The command requests a read-only UDZO
container and verifies its checksum, then binds its observed bytes with SHA-256.
The receipt sits outside the image and does not create a self-referential hash.
It records the full input application graph/modes and Runtime identity.
No mounted/extracted image graph comparison has been performed. HFS+ copy,
ownership and extraction behavior must be qualified on a separately admitted
real image before claiming Runtime mode/graph preservation after installation.
The current macOS manual prefers `diskutil image` for new image features;
this narrow lane uses the existing documented HFS+/UDZO `hdiutil` route.

## Architecture and future signing order

The initial lane is Apple Silicon (M1/M4 architecture), subject to actual
minimum-OS and native qualification. Intel/universal is deferred: the existing
Runtime target matrix, host selection and Mach-O verifier admit macOS ARM64
only. A universal Swift wrapper alone would not provide a matching Intel
Runtime. Intel needs a new core target/build/linkage and native proof track,
not a cheap packaging switch.

Paid Apple enrollment, Developer ID and notarization are deferred. A later
separately approved distribution implementation would need this order:

1. Build the selected native artifacts and sign each Runtime Mach-O **before**
   final Runtime manifest hashes. The concrete builder extension point is after
   `stage_release_binaries` and before the `migration-stream-bundle` assembly in
   `scripts/build-assisted-runtime-bundle.sh`. Revalidate the signed copies and
   preserve source/compiled identity; do not codesign sealed Runtime members.
2. Sign the Swift wrapper before copying it. Assemble the application with the
   sealed signed Runtime intact, then sign the outer application envelope
   without changing Runtime bytes. Recheck its complete graph and mode/hash
   identities. Account for outer `_CodeSignature` metadata in packaging tests.
3. Notarization/stapling of the application, if selected, must precede final
   application inventory and image creation. Sign/notarize/staple the disk image
   only afterward, and compute final DMG hash/receipt after every byte-changing
   step. Requalify mounted payload and distribution behavior separately.

None of those signing/network/stapling operations exists in this implementation.
There is no keychain or certificate discovery and no automatic registration.

## Synthetic source verification

```sh
python3 -B scripts/package-macos-engineering-desktop-self-test.py
```

These tests run the shared application assembler and packaging control flow with
fake Runtime files and a small nonrunnable Mach-O header, injected Runtime
reports, injected image commands and an injected publication seam. Process
creation is blocked. Tiny fake image bytes are not a DMG. The test's `.app`-named
directory and plist are ordinary private temporary test data; no application,
image mount, native event, signing operation or OS registration is invoked.
The injected publisher proves control flow, not native atomic publication.
Existing assembler/native atomic and UI/process proofs are not replayed.
