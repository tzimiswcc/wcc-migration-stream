# One-download unsigned Windows engineering installer

The source generator `scripts/package-windows-desktop.py` prepares one NSIS
`Migration Stream Setup.exe` containing both native x64 and ARM64 engineering
payloads. The installer chooses the OS's native architecture automatically;
there is no architecture picker. This is a bootstrap installer containing two
native payloads, not a Windows fat executable or an MSIX package.

[NSIS is free for any use][nsis-free]. No paid certificate, signing, account,
service or notarization is selected. The output is unsigned engineering material,
not a public-support/release decision. SmartScreen, organizational policy and
actual install/Open With behavior remain unqualified; no security bypass,
Defender exclusion, trust-store modification or unblock operation is provided.

## Trusted inputs and passive binding

Two complete native engineering layouts and their original successful
`migration-stream/windows-engineering-qualification/v1` receipts are required.
Each layout contains exactly `migration-stream-document-open.exe` beside an
intact `Runtime`. The caller supplies each receipt's SHA-256 from a trusted
external source, plus the expected common source commit, Git tree, source
snapshot and product version. The product version is bound through the
receipt-pinned canonical Runtime manifest; the qualification receipt does not
invent a product-version field.

The generator checks the current pinned recipe fields, closed x64/ARM64 target
pairs, receipt/file/self-manifest hashes, CLEAN product identity, complete
file/directory graph, member sizes/modes, path ordering/case collisions and
same-capture PE32+ machine headers. Native executables, outer document launcher
and frontend identity remain distinct. Both native layouts must share the
expected source identities and frontend snapshot. Source, copied payload,
compiler, rendered script, template and receipt identities are replayed after
compilation; changes refuse publication.

This is **passive byte/PE/graph binding to already trusted native receipts**.
It does not run either Runtime, recreate its semantic verifier, authenticate an
unsigned JSON receipt, prove compiled provenance, or turn `status: PASSED` into
new native evidence. Receipt hashes identify bytes; the operator must establish
that they came from the admitted native builds. The generator deliberately
reports `PASSIVE_RECEIPT_BYTE_BINDING_ONLY` and installer execution unqualified.
Synthetic fixtures are not genuine native receipts or acceptance records.

Real file/directory, single-link and no-reparse checks apply. On Windows,
[FindFirstStreamW/FindNextStreamW][streams] reject every nondefault data stream
without reading or stripping it; unsupported/unknown enumeration refuses.
Byte reads explicitly use Windows `O_BINARY` so CRLF and control-Z are payload
bytes. Path and descriptor fingerprints each retain their own complete freshness
checks. Their common identity binds the full device/file ID, type, size, mtime,
link count, readonly/write bits, Windows attributes and available birthtime.
Only Windows' filename-synthesized executable bits are normalized between those
interfaces; their differently defined `ctime` fields are checked within each
interface. Unknown asymmetric metadata refuses. This follows [CPython's path
mode/ctime implementation][cpython-path] and [descriptor implementation][cpython-fd],
plus the documented [binary-open requirement][python-open]. The first hosted
source failure did not log the actual differing tuple fields; these are verified
API differences, not an observed field-by-field diagnosis of that failure.

An ordinary local drive namespace is required. Traversal, ADS/device names,
trailing-dot/space aliases, links, unknown files and overlapping physical input
or output ancestors refuse. Unicode/interior spaces are allowed in compiler
source paths, while `$`, `!`, quotes and nonprintable characters are refused to
avoid NSIS variable/preprocessor injection. No silent escaping or normalization
is substituted. Runtime member paths retain the existing bounded ASCII subset.
Manifest mode strings remain bound; existing Windows readonly/archive/normal
attributes are not promoted into a new Runtime authority gate. Portable source
mode checks/copies do not claim Windows attribute restoration.

Bounds include 1024 Runtime members, 128 MiB per native member, 512 MiB Runtime
member total, 642 MiB per layout, 4096 graph entries, depth 64, 512-byte member
paths, 1 MiB JSON inputs, 8 MiB generated NSIS and 1.5 GiB final Setup. Frontend
member/count/total limits are retained. These are input/observed-output bounds,
not filesystem or memory quotas. Large accepted files can require substantial
compiler and Python memory; actual build resource admission is separate.

## Explicit compiler and create-only output

Required CLI flags are:

```text
--x64-layout ABSOLUTE_LAYOUT --x64-receipt ABSOLUTE_RECEIPT --x64-receipt-sha256 TRUSTED_SHA256
--arm64-layout ABSOLUTE_LAYOUT --arm64-receipt ABSOLUTE_RECEIPT --arm64-receipt-sha256 TRUSTED_SHA256
--source-commit EXPECTED_COMMIT --source-tree EXPECTED_TREE
--source-snapshot-sha256 EXPECTED_SNAPSHOT --product-version EXPECTED_VERSION
--makensis ABSOLUTE_TRUSTED_MAKENSIS_EXE --makensis-sha256 EXPECTED_COMPILER_SHA256
--output-directory ABSOLUTE_NEW_DIRECTORY
```

The packager itself performs no dependency installation or download. Its
compiler path/hash must identify the prepared direct compiler, not the release
root's child-spawning `makensis.exe` stub. The shared
[`prepare-nsis-tool.py`](../scripts/prepare-nsis-tool.py) explicitly fetches the
official portable NSIS 3.13 ZIP into one new runner-temp namespace only when
separately invoked in an admitted build job. It neither executes an installer
nor changes PATH, registry, services or global configuration. There is no
installed-path search or alternate download fallback.

The source pins the 2,363,925-byte archive SHA-256
`ba63dffc4410ee89193e1cb5a41989991bd77c61068da17e3156d136b7b0b3d8`,
441 entries and 7,141,489 expanded bytes, plus nine compiler/header/plugin/stub
hashes. Preparation performs one bounded HTTPS fetch (10-second I/O timeout,
60-second observed whole-fetch budget, at most three constrained HTTPS
SourceForge mirror redirects), checks that hash before creating extraction
staging, then refuses unsafe names, aliases, links, special files, excessive
counts/depth/bytes and overwrite. Unknown failures retain only owned partials;
there is no retry. Publisher archive identity is not a signature or
supply-chain attestation. The archive pin was checked against the official release ZIP before this
source change; local source tests use only fake ZIPs, never that distribution.

The returned compiler is exactly `nsis-3.13/Bin/makensis.exe` (468,992 bytes,
PE32/x86, SHA-256
`65343c225ec88393cc4453b0963caf63f763f42f48cb55736d51893ff2e29d4a`).
[Official 3.13 source][compiler-root] derives its Include/Stubs/Plugins root from
the parent of the executable directory; Windows defaults do not use a compiled
constant resource path. Preserving the complete release graph therefore needs
no inherited NSISDIR override or root launcher. The existing cleared compiler
environment stays unchanged. The helper and callers validate the full prepared
file graph/bytes and receipt again after compilation. They reuse the packager's
reviewed Windows binary-read/file-identity/ADS policy rather than treating Unix
lstat/fstat identifiers as interchangeable on Windows.

Existing `Unicode true`, `NSIS_CHAR_SIZE == 2` and `NSIS_PTR_SIZE == 4` template
guards remain. The standard x86 Unicode bootstrap chooses the application's
native payload; actual NSIS/template compatibility still requires the future
synthetic compiler proof. The preparation metadata records no executable call
by the helper; it is not installer or native-lifecycle qualification.

The production compiler runs on Windows only with exact arguments
`/NOCONFIG /V2 /INPUTCHARSET UTF8 SCRIPT`, only trusted OS-derived
`SystemRoot` (for loader/SxS compatibility) and private per-run `TEMP`/`TMP`
environment entries, null stdin and
no shell or extra command option. `GetSystemWindowsDirectoryW` supplies that
validated real system directory; `TEMP`/`TMP` point only to the owned compiler
workspace. No ambient environment, PATH or user profile is inherited. [The compiler options][compiler] disable
configuration-file inclusion and select bounded diagnostics. A 300-second
observed deadline, fair pipe peeks and 64 KiB retained stdout/stderr limits apply;
cleanup addresses only the retained direct compiler child. Unknown metadata or
cleanup refuses, with no retry. OS calls are not real-time interruptions and
no descendant-process sandbox is claimed. Bounded raw compiler diagnostics stay
in the private compiler-input directory, never in the output receipt.

Fresh sibling private directories hold copied payloads, owner markers and the
rendered script. These inputs and failures are retained for inspection rather
than recursively deleting a compiler-populated namespace. The visible output
contains only Setup and a bounded byte/provenance receipt. Production Windows
`os.rename` publishes the new directory without replacing an existing target;
there is no production POSIX replacement fallback. A final freshness failure
preserves visible output and reports failure.

## Installer and removal policy

The [NSIS template](../native/windows/desktop-installer.nsi.in) sets Unicode,
CRC checking and `RequestExecutionLevel user`. Official
[`IsNativeAMD64`/`IsNativeARM64` macros][native-cpu] select only the matching
payload; other CPUs refuse. Installation always derives
`$LOCALAPPDATA\Programs\MigrationStream` from the current user. There is no
directory picker or accepted command-line root override. Existing installation
roots or private registrations refuse; this lane does not upgrade or overwrite.
Ancestor reparse checks precede writes/deletes, the new root uses an exclusive
CreateDirectory result, and extraction disables overwrite. These are observed
filesystem guards, not a handle-rooted sandbox against a malicious same-user
namespace race. Actual junction/race behavior requires native installation tests.

Per-user registration adds the private ProgID and a named `.mstream`
`OpenWithProgids` entry. It does not replace the extension default or UserChoice.
An optional Start Menu shortcut invokes an installed copy of the installer in
explicit `/open`-only mode. That mode verifies the fixed installation marker,
opens the built-in [file selection dialog][picker], then uses `ExecWait` with the
fixed document executable and exactly one quoted selected operand. There is no
Cmd/PowerShell/shell interpolation, project fallback or automatic app startup.
Long command strings refuse before launch; native Unicode/path behavior still
requires actual qualification. The existing document coordinator and Runtime
retain all project, ownership and lifecycle authority.

The uninstaller accepts only its fixed current-user root and matching owner /
architecture marker. Its compiled lists delete only the installed architecture's
literal payload names and empty directories, never wildcard/recursive trees or
user projects/runtime state. In-use files, unexpected contents and uncertain
paths leave partials. Private registry values are removed only if their current
values still match; Open With removal additionally checks the private command
and REG_NONE type/length. Changed/default/user-selected values are left alone.
There are no services, scheduled tasks, elevation or automatic lifecycle stops.
Install/uninstall, Explorer refresh, locking, Open With and launch behavior are
source intent until separately tested on Windows; tests do not qualify them.

## Offline source tests

```sh
python3 -B scripts/package-windows-desktop-self-test.py
```

Tests generate only tiny synthetic PE headers, manifests and receipts, exercise
the actual passive validator and NSIS source rendering, and inject a fake
compiler writing a nonrunnable PE header. Real process creation is blocked.
No Runtime, NSIS compiler, installer, registry, dialog or browser process runs.
These default tests do not qualify actual NSIS compiler/parser, Windows
stream/pipe API or installer behavior. Existing Windows/ARM/Runtime suites are
not replayed by this test script.

The source workflow also compiles one tiny synthetic installer on its x64
Windows runner using the fixed, already-installed NSIS distribution. The
explicit test-only `--native-compile-only` route logs the compiler identity and
uses the production bounded compiler helper. Missing tools fail without a
download or installation fallback. Generated payloads are fake PE headers;
the resulting Setup is never executed, uploaded or treated as a product build.
This checks actual template/compiler grammar, while registry, file selection,
installation, native payload and lifecycle behavior remain unqualified. The
default fourteen tests continue to block process creation.
The explicit native fixture retains its generated namespace for the ephemeral
job to dispose of, including on uncertain child cleanup. It does not recursively
delete a compiler-populated tree; failure diagnostics use only the two known
compiler logs through bounded real-file reads.

[nsis-free]: https://nsis.sourceforge.io/Docs/Chapter1.html
[compiler]: https://nsis.sourceforge.io/Docs/Chapter3.html
[native-cpu]: https://github.com/kichik/nsis/blob/master/Include/x64.nsh
[picker]: https://nsis.sourceforge.io/Docs/nsDialogs/Readme.html#selectfiledialog
[streams]: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findfirststreamw

[cpython-path]: https://github.com/python/cpython/blob/v3.12.10/Modules/posixmodule.c#L1777-L1798
[cpython-fd]: https://github.com/python/cpython/blob/v3.12.10/Python/fileutils.c#L1026-L1047
[python-open]: https://docs.python.org/3.12/library/os.html#os.open

[compiler-root]: https://github.com/nsis-dev/nsis/blob/v313/Source/build.cpp#L407-L426
