# Windows ARM64 owner-device verification (Yoga)

This is a device-evidence runbook, not a release procedure. The matrix entry
for `migration-stream-local-windows-arm64` remains
`NATIVE_LIFECYCLE_AND_DEVICE_REQUIRED` until Yoga has run this exact proof and
the resulting CI-independent evidence has been reviewed.

## Preconditions

Use a native Windows ARM64 device. Confirm both values before starting:

```powershell
$env:PROCESSOR_ARCHITECTURE
& rustc -vV | Select-String '^host:'
```

They must be `ARM64` and `host: aarch64-pc-windows-msvc`. Do not use emulation
or an x64 runner for either proof lane below.

## Local implementation acceptance candidate

This lane is for repair and acceptance-candidate evidence on the Yoga. It is
not the later replay of an approved release input. From one isolated local
branch, first run the non-elevated formatting and strict native lint gates:

```powershell
cargo fmt --all -- --check
cargo clippy --locked --offline --workspace --all-targets --all-features `
  --target aarch64-pc-windows-msvc -- -D warnings
```

The complete Rust workspace and the release-corpus self-test exercise Windows
file and directory links. If the ordinary token cannot create both, open one
fresh native ARM64 Administrator PowerShell only for these test gates. Start a
private transcript, enter the exact repository, and run this bounded preflight
and gate sequence in that same process. The fixture is held through no-follow
native handles that bind its creation-time FileIds and owner/group/DACLs. No
deletion begins until every retained fact and the complete namespace replay;
any ambiguity retains the fixture and fails the gate without a path-delete
fallback:

```powershell
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-Location 'C:\dev\migration-stream'

if (-not ('MigrationStreamExactFixtureObject' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using Microsoft.Win32.SafeHandles;

public sealed class MigrationStreamExactFixtureObject : IDisposable
{
    [StructLayout(LayoutKind.Sequential)]
    private struct BY_HANDLE_FILE_INFORMATION
    {
        public uint FileAttributes;
        public System.Runtime.InteropServices.ComTypes.FILETIME CreationTime;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastAccessTime;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWriteTime;
        public uint VolumeSerialNumber;
        public uint FileSizeHigh;
        public uint FileSizeLow;
        public uint NumberOfLinks;
        public uint FileIndexHigh;
        public uint FileIndexLow;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct FILE_DISPOSITION_INFO
    {
        [MarshalAs(UnmanagedType.Bool)]
        public bool DeleteFile;
    }

    private sealed class Fact
    {
        public readonly uint Attributes;
        public readonly uint ReparseTag;
        public readonly uint Links;
        public readonly string Key;

        public Fact(uint attributes, uint reparseTag, uint links, string key)
        {
            Attributes = attributes;
            ReparseTag = reparseTag;
            Links = links;
            Key = key;
        }
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(
        string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CreateDirectoryW(string path, IntPtr security);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandle(
        SafeFileHandle file, out BY_HANDLE_FILE_INFORMATION information);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandleEx(
        SafeFileHandle file, int informationClass,
        [Out] byte[] information, uint bufferSize);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetFileInformationByHandle(
        SafeFileHandle file, int informationClass,
        ref FILE_DISPOSITION_INFO information, uint bufferSize);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ReadFile(
        SafeFileHandle file, [Out] byte[] buffer, uint bytesToRead,
        out uint bytesRead, IntPtr overlapped);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetFilePointerEx(
        SafeFileHandle file, long distance, out long newPosition,
        uint moveMethod);

    [DllImport("advapi32.dll")]
    private static extern uint GetSecurityInfo(
        SafeFileHandle handle, uint objectType, uint securityInformation,
        out IntPtr owner, out IntPtr group, out IntPtr dacl,
        out IntPtr sacl, out IntPtr securityDescriptor);

    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ConvertSecurityDescriptorToStringSecurityDescriptorW(
        IntPtr securityDescriptor, uint revision, uint securityInformation,
        out IntPtr stringSecurityDescriptor, out uint stringLength);

    [DllImport("kernel32.dll")]
    private static extern IntPtr LocalFree(IntPtr memory);

    private const uint DeleteAccess = 0x00010000;
    private const uint ReadControl = 0x00020000;
    private const uint FileReadData = 0x1;
    private const uint FileReadAttributes = 0x80;
    private const uint FileShareRead = 0x1;
    private const uint FileShareWrite = 0x2;
    private const uint FileShareDelete = 0x4;
    private const uint OpenExisting = 3;
    private const uint FileFlagBackupSemantics = 0x02000000;
    private const uint FileFlagOpenReparsePoint = 0x00200000;
    private const uint FileAttributeDirectory = 0x10;
    private const uint FileAttributeReparsePoint = 0x400;
    private const uint IoReparseTagSymlink = 0xA000000C;
    private const int FileDispositionInfo = 4;
    private const int FileAttributeTagInfo = 9;
    private const int FileIdInfo = 18;
    private const uint FileBegin = 0;
    private const uint OwnerGroupDaclInformation = 0x7;
    private const uint SeFileObject = 1;

    private readonly string path;
    private readonly bool expectedDirectory;
    private readonly bool expectedReparse;
    private readonly bool requireSingleLink;
    private readonly bool deletable;
    private readonly Fact expected;
    private SafeFileHandle handle;
    private bool deleted;

    private MigrationStreamExactFixtureObject(
        string path, SafeFileHandle handle, Fact expected,
        bool expectedDirectory, bool expectedReparse,
        bool deletable, bool requireSingleLink)
    {
        this.path = path;
        this.handle = handle;
        this.expected = expected;
        this.expectedDirectory = expectedDirectory;
        this.expectedReparse = expectedReparse;
        this.deletable = deletable;
        this.requireSingleLink = requireSingleLink;
    }

    public static void CreateDirectoryNew(string path)
    {
        string full = Path.GetFullPath(path);
        if (!CreateDirectoryW(full, IntPtr.Zero))
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "exact fixture directory creation failed");
    }

    public static MigrationStreamExactFixtureObject Capture(
        string path, bool expectedDirectory, bool expectedReparse,
        bool deletable, bool requireSingleLink)
    {
        string full = Path.GetFullPath(path);
        SafeFileHandle retained = Open(
            full, deletable, false, !expectedDirectory && !expectedReparse);
        try
        {
            Fact fact = ReadFact(retained);
            RequireShape(
                fact, expectedDirectory, expectedReparse, requireSingleLink);
            return new MigrationStreamExactFixtureObject(
                full, retained, fact, expectedDirectory, expectedReparse,
                deletable, requireSingleLink);
        }
        catch
        {
            retained.Dispose();
            throw;
        }
    }

    public void Verify()
    {
        if (deleted)
            throw new InvalidDataException("exact fixture object was deleted");

        Fact retained = ReadFact(handle);
        RequireShape(
            retained, expectedDirectory, expectedReparse, requireSingleLink);
        if (!String.Equals(retained.Key, expected.Key, StringComparison.Ordinal))
            throw new InvalidDataException("retained fixture authority changed");

        using (SafeFileHandle named = Open(
            path, false, true, !expectedDirectory && !expectedReparse))
        {
            Fact replayed = ReadFact(named);
            RequireShape(
                replayed, expectedDirectory, expectedReparse, requireSingleLink);
            if (!String.Equals(replayed.Key, expected.Key, StringComparison.Ordinal))
                throw new InvalidDataException("named fixture authority changed");
        }
    }

    public void DeleteExact()
    {
        if (!deletable)
            throw new InvalidOperationException("fixture authority is retention-only");

        Verify();
        FILE_DISPOSITION_INFO disposition =
            new FILE_DISPOSITION_INFO { DeleteFile = true };
        uint size = checked((uint)Marshal.SizeOf(typeof(FILE_DISPOSITION_INFO)));
        if (!SetFileInformationByHandle(
            handle, FileDispositionInfo, ref disposition, size))
        {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "exact fixture deletion failed");
        }

        handle.Dispose();
        deleted = true;
        AssertAbsent(path);
    }

    public string ContentSha256()
    {
        if (expectedDirectory || expectedReparse)
            throw new InvalidOperationException(
                "fixture content hash requires a regular file");

        Verify();
        long ignored;
        if (!SetFilePointerEx(handle, 0, out ignored, FileBegin))
            throw new Win32Exception(Marshal.GetLastWin32Error());

        byte[] buffer = new byte[8192];
        using (SHA256 sha256 = SHA256.Create())
        {
            while (true)
            {
                uint read;
                if (!ReadFile(
                    handle, buffer, checked((uint)buffer.Length),
                    out read, IntPtr.Zero))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                }
                if (read == 0)
                    break;
                sha256.TransformBlock(
                    buffer, 0, checked((int)read), null, 0);
            }
            sha256.TransformFinalBlock(new byte[0], 0, 0);
            return BitConverter.ToString(sha256.Hash)
                .Replace("-", "")
                .ToUpperInvariant();
        }
    }

    private static SafeFileHandle Open(
        string path, bool requestDelete, bool permitDeleteSharing,
        bool requestContentRead)
    {
        uint access = FileReadAttributes | ReadControl;
        if (requestDelete)
            access |= DeleteAccess;
        if (requestContentRead)
            access |= FileReadData;

        uint share = FileShareRead | FileShareWrite;
        if (permitDeleteSharing)
            share |= FileShareDelete;

        SafeFileHandle opened = CreateFileW(
            path, access, share, IntPtr.Zero, OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero);
        if (opened.IsInvalid)
        {
            int error = Marshal.GetLastWin32Error();
            opened.Dispose();
            throw new Win32Exception(error, "exact fixture open failed");
        }
        return opened;
    }

    private static Fact ReadFact(SafeFileHandle handle)
    {
        BY_HANDLE_FILE_INFORMATION information;
        if (!GetFileInformationByHandle(handle, out information))
            throw new Win32Exception(Marshal.GetLastWin32Error());

        byte[] fileId = new byte[24];
        if (!GetFileInformationByHandleEx(
            handle, FileIdInfo, fileId, checked((uint)fileId.Length)))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }

        byte[] tag = new byte[8];
        if (!GetFileInformationByHandleEx(
            handle, FileAttributeTagInfo, tag, checked((uint)tag.Length)))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }

        ulong volume = BitConverter.ToUInt64(fileId, 0);
        byte[] identifier = new byte[16];
        Buffer.BlockCopy(fileId, 8, identifier, 0, identifier.Length);
        string identifierHex =
            BitConverter.ToString(identifier).Replace("-", "").ToLowerInvariant();
        uint reparseTag = BitConverter.ToUInt32(tag, 4);
        ulong creationTime =
            ((ulong)(uint)information.CreationTime.dwHighDateTime << 32) |
            (uint)information.CreationTime.dwLowDateTime;
        string sddl = ReadOwnerGroupDaclSddl(handle);

        string key = String.Format(
            "{0:x16}:{1}:{2}:{3:x8}:{4:x8}:{5:x16}:{6}",
            volume, identifierHex, information.NumberOfLinks,
            information.FileAttributes, reparseTag, creationTime, sddl);
        return new Fact(
            information.FileAttributes,
            reparseTag,
            information.NumberOfLinks,
            key);
    }

    private static string ReadOwnerGroupDaclSddl(SafeFileHandle handle)
    {
        IntPtr owner, group, dacl, sacl, descriptor;
        uint status = GetSecurityInfo(
            handle, SeFileObject, OwnerGroupDaclInformation,
            out owner, out group, out dacl, out sacl, out descriptor);
        if (status != 0)
            throw new Win32Exception(checked((int)status));

        IntPtr text = IntPtr.Zero;
        try
        {
            uint length;
            if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor, 1, OwnerGroupDaclInformation,
                out text, out length))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            string rendered = Marshal.PtrToStringUni(text);
            if (String.IsNullOrEmpty(rendered))
                throw new InvalidDataException("fixture security descriptor is empty");
            return rendered;
        }
        finally
        {
            if (text != IntPtr.Zero)
                LocalFree(text);
            if (descriptor != IntPtr.Zero)
                LocalFree(descriptor);
        }
    }

    private static void RequireShape(
        Fact fact, bool directory, bool reparse, bool singleLink)
    {
        bool observedDirectory =
            (fact.Attributes & FileAttributeDirectory) != 0;
        bool observedReparse =
            (fact.Attributes & FileAttributeReparsePoint) != 0;

        if (observedDirectory != directory || observedReparse != reparse)
            throw new InvalidDataException("fixture object type changed");
        if (reparse && fact.ReparseTag != IoReparseTagSymlink)
            throw new InvalidDataException("fixture reparse tag is not a symlink");
        if (!reparse && fact.ReparseTag != 0)
            throw new InvalidDataException("non-reparse fixture has a reparse tag");
        if (singleLink && fact.Links != 1)
            throw new InvalidDataException("fixture object has a hard-link alias");
    }

    private static void AssertAbsent(string path)
    {
        SafeFileHandle probe = CreateFileW(
            path, 0, FileShareRead | FileShareWrite | FileShareDelete,
            IntPtr.Zero, OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero);
        if (!probe.IsInvalid)
        {
            probe.Dispose();
            throw new IOException("deleted fixture name still exists");
        }

        int error = Marshal.GetLastWin32Error();
        probe.Dispose();
        if (error != 2 && error != 3)
            throw new Win32Exception(error, "fixture absence was not proven");
    }

    public void Dispose()
    {
        if (handle != null)
            handle.Dispose();
    }
}
'@
}

function Assert-ExactFixtureAuthorities([object[]]$Authorities) {
    foreach ($authority in $Authorities) { $authority.Verify() }
}

function Assert-ExactFixtureInventory([string[]]$ExpectedNames) {
    $expected = @($ExpectedNames | Sort-Object -CaseSensitive)
    $actual = @(
        Get-ChildItem -LiteralPath $proofRoot -Force |
            ForEach-Object Name |
            Sort-Object -CaseSensitive
    )
    if (
        $actual.Count -ne $expected.Count -or
        ($actual -join "`n") -cne ($expected -join "`n")
    ) {
        throw 'fixture namespace differs from its retained inventory'
    }
}

$gitBefore = (& git status --porcelain=v1 --untracked-files=all) -join "`n"
if ($LASTEXITCODE -ne 0) { throw 'initial Git state inspection failed' }
$tempRoot = [IO.Path]::TrimEndingDirectorySeparator(
    (Resolve-Path -LiteralPath ([IO.Path]::GetTempPath())).Path
)
$proofName = "migration-stream-arm64-link-proof-$([Guid]::NewGuid())"
$proofRoot = Join-Path $tempRoot $proofName
$targetFile = Join-Path $proofRoot 'target.txt'
$targetDirectory = Join-Path $proofRoot 'target-directory'
$fileLink = Join-Path $proofRoot 'file-link'
$directoryLink = Join-Path $proofRoot 'directory-link'

$tempAuthority = $null
$rootAuthority = $null
$fileAuthority = $null
$directoryAuthority = $null
$fileLinkAuthority = $null
$directoryLinkAuthority = $null
$proofCreated = $false
$cleanupStarted = $false
$fixtureAuthorityReplay = $false
$fixtureCleanupProven = $false
$fixtureDisposition = 'NOT_CREATED'
$targetFileSha256 = $null
$fileLinkTarget = $null
$directoryLinkTarget = $null
$rustExit = -1
$pythonExit = -1

$tempAuthority = [MigrationStreamExactFixtureObject]::Capture(
    $tempRoot, $true, $false, $false, $false
)

try {
    [MigrationStreamExactFixtureObject]::CreateDirectoryNew($proofRoot)
    $proofCreated = $true
    $fixtureDisposition = 'RETAINED_PENDING_EXACT_CLEANUP'
    $rootAuthority = [MigrationStreamExactFixtureObject]::Capture(
        $proofRoot, $true, $false, $true, $true
    )

    [MigrationStreamExactFixtureObject]::CreateDirectoryNew($targetDirectory)
    $directoryAuthority = [MigrationStreamExactFixtureObject]::Capture(
        $targetDirectory, $true, $false, $true, $true
    )

    $bytes = [Text.Encoding]::UTF8.GetBytes('owned-link-target')
    $stream = [IO.File]::Open(
        $targetFile,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None
    )
    try {
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
    $targetFileSha256 = (Get-FileHash -LiteralPath $targetFile -Algorithm SHA256).Hash
    $fileAuthority = [MigrationStreamExactFixtureObject]::Capture(
        $targetFile, $false, $false, $true, $true
    )

    New-Item -ItemType SymbolicLink -Path $fileLink -Target $targetFile | Out-Null
    $fileLinkItem = Get-Item -LiteralPath $fileLink -Force
    $fileLinkTarget = [IO.Path]::GetFullPath([string]$fileLinkItem.LinkTarget)
    if (-not [string]::Equals($fileLinkTarget, $targetFile, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'file symlink target differs immediately after creation'
    }
    $fileLinkAuthority = [MigrationStreamExactFixtureObject]::Capture(
        $fileLink, $false, $true, $true, $true
    )

    New-Item -ItemType SymbolicLink -Path $directoryLink `
        -Target $targetDirectory | Out-Null
    $directoryLinkItem = Get-Item -LiteralPath $directoryLink -Force
    $directoryLinkTarget = [IO.Path]::GetFullPath([string]$directoryLinkItem.LinkTarget)
    if (-not [string]::Equals($directoryLinkTarget, $targetDirectory, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'directory symlink target differs immediately after creation'
    }
    $directoryLinkAuthority = [MigrationStreamExactFixtureObject]::Capture(
        $directoryLink, $true, $true, $true, $true
    )

    cargo test --locked --offline --workspace --all-features `
        --target aarch64-pc-windows-msvc
    $rustExit = $LASTEXITCODE
    python -B scripts/release-corpus-vocabulary-gate-self-test.py
    $pythonExit = $LASTEXITCODE
} finally {
    $cleanupFailure = $null
    try {
        if (
            -not $proofCreated -or
            $null -eq $rootAuthority -or
            $null -eq $fileAuthority -or
            $null -eq $directoryAuthority -or
            $null -eq $fileLinkAuthority -or
            $null -eq $directoryLinkAuthority
        ) {
            throw 'complete exact fixture authority was not captured'
        }

        $allAuthorities = @(
            $tempAuthority,
            $rootAuthority,
            $fileAuthority,
            $directoryAuthority,
            $fileLinkAuthority,
            $directoryLinkAuthority
        )
        Assert-ExactFixtureAuthorities $allAuthorities
        Assert-ExactFixtureInventory @(
            'directory-link',
            'file-link',
            'target-directory',
            'target.txt'
        )
        if (
            @(Get-ChildItem -LiteralPath $targetDirectory -Force).Count -ne 0 -or
            $fileAuthority.ContentSha256() -cne $targetFileSha256 -or
            -not [string]::Equals(
                [IO.Path]::GetFullPath([string](Get-Item -LiteralPath $fileLink -Force).LinkTarget),
                $fileLinkTarget,
                [StringComparison]::OrdinalIgnoreCase
            ) -or
            -not [string]::Equals(
                [IO.Path]::GetFullPath([string](Get-Item -LiteralPath $directoryLink -Force).LinkTarget),
                $directoryLinkTarget,
                [StringComparison]::OrdinalIgnoreCase
            )
        ) {
            throw 'fixture contents differ from their creation-time contract'
        }
        $fixtureAuthorityReplay = $true
        $cleanupStarted = $true

        Assert-ExactFixtureAuthorities $allAuthorities
        Assert-ExactFixtureInventory @(
            'directory-link', 'file-link', 'target-directory', 'target.txt'
        )
        $fileLinkAuthority.DeleteExact()

        Assert-ExactFixtureAuthorities @(
            $tempAuthority, $rootAuthority, $fileAuthority,
            $directoryAuthority, $directoryLinkAuthority
        )
        Assert-ExactFixtureInventory @(
            'directory-link', 'target-directory', 'target.txt'
        )
        $directoryLinkAuthority.DeleteExact()

        Assert-ExactFixtureAuthorities @(
            $tempAuthority, $rootAuthority, $fileAuthority, $directoryAuthority
        )
        Assert-ExactFixtureInventory @('target-directory', 'target.txt')
        if ($fileAuthority.ContentSha256() -cne $targetFileSha256) {
            throw 'fixture target file changed before exact deletion'
        }
        $fileAuthority.DeleteExact()

        Assert-ExactFixtureAuthorities @(
            $tempAuthority, $rootAuthority, $directoryAuthority
        )
        Assert-ExactFixtureInventory @('target-directory')
        if (@(Get-ChildItem -LiteralPath $targetDirectory -Force).Count -ne 0) {
            throw 'fixture target directory is not empty'
        }
        $directoryAuthority.DeleteExact()

        Assert-ExactFixtureAuthorities @($tempAuthority, $rootAuthority)
        Assert-ExactFixtureInventory @()
        $rootAuthority.DeleteExact()
        $tempAuthority.Verify()
        $fixtureCleanupProven = $true
        $fixtureDisposition = 'REMOVED_BY_EXACT_HANDLE'
    } catch {
        $cleanupFailure = $_
        $fixtureDisposition = if (-not $proofCreated) {
            'NOT_CREATED'
        } elseif (-not $cleanupStarted) {
            'RETAINED_AFTER_UNPROVEN_CLEANUP'
        } else {
            'PARTIAL_AFTER_EXACT_CLEANUP_FAILURE'
        }
    } finally {
        foreach ($authority in @(
            $directoryLinkAuthority,
            $fileLinkAuthority,
            $directoryAuthority,
            $fileAuthority,
            $rootAuthority,
            $tempAuthority
        )) {
            if ($null -ne $authority) { $authority.Dispose() }
        }
    }

    if ($null -ne $cleanupFailure) {
        throw [InvalidOperationException]::new(
            'link-proof cleanup stopped; unmatched material was retained',
            $cleanupFailure.Exception
        )
    }
}
$gitAfter = (& git status --porcelain=v1 --untracked-files=all) -join "`n"
if ($LASTEXITCODE -ne 0) { throw 'final Git state inspection failed' }
if ($gitAfter -cne $gitBefore) { throw 'elevated gates changed Git-visible source state' }
if ($rustExit -ne 0 -or $pythonExit -ne 0) { throw 'one or more elevated native gates failed' }
[pscustomobject]@{
    fileLinkPreflight = $true
    directoryLinkPreflight = $true
    rustExitCode = $rustExit
    releaseCorpusSelfTestExitCode = $pythonExit
    fixtureAuthorityReplay = $fixtureAuthorityReplay
    fixtureCleanupProven = $fixtureCleanupProven
    fixtureDisposition = $fixtureDisposition
    gitStatePreserved = $true
}
```

Close the elevated process after recording the exact test counts, zero exit
codes, authority replay, exact-handle fixture disposition, and unchanged Git
state. Never run bundle or lifecycle acceptance elevated: the ordinary owner
token is part of the SID, DACL, listener, Job Object, and cleanup proof.

Then run the shared Windows harness non-elevated with the exact ARM64 target:

```powershell
.\scripts\windows-native-x64-acceptance.ps1 `
  -Target aarch64-pc-windows-msvc `
  -AllowUncommittedSnapshot
```

The switch records an explicit uncommitted source snapshot; it does not call
that snapshot clean or approved. The harness must select harness-owned ports
and directories, build with the native ARM64 Rust host, require PE machine
`0xAA64` for every release and shipped executable, bind the archive and exact
checksum sidecar to the assembler result, unpack through the manifest graph,
and verify both root and controller entrypoints. It must then prove governed
Object Workspace initialization, idempotent start, restart process replacement
with live `OWNED` authority continuity, authenticated stop and exact `STOPPED`
process/listener/session proof, stopped-only integrity followed by another
unchanged `STOPPED` replay, foreign-listener refusal without state mutation,
and bounded cleanup. Evidence must suppress tokens, paths, and process
identifiers.

Retain a separate value-suppressed local gate ledger with the exact source
commit/state, native host and tool versions, formatting/Clippy/test commands
and counts, both link-preflight results, fixture authority replay and exact
handle disposition, manifest/archive hashes, the post-restart `OWNED` status,
authenticated `STOPPED` transition, pre/post-integrity `STOPPED` statuses and
unchanged workspace/session digests, rejection codes, and cleanup disposition.

On Windows, bounded cleanup means the authenticated runtime is stopped, owned
process handles are signaled, listeners are absent or released, protected
active records are absent, and persistent lock authority replays cleanly. The
harness does not path-recursively delete its bundle/workspace proof tree: that
could cross a concurrent namespace replacement. It instead records
`RETAINED_FOR_EXACT_RECOVERY` plus a value-suppressed FileId, DACL, attribute,
name, size, and content digest. Any unproven runtime cleanup uses the distinct
`RETAINED_AFTER_UNPROVEN_CLEANUP` disposition and cannot pass acceptance.

After the single reviewed local commit, rerun without
`-AllowUncommittedSnapshot`. Only that clean rerun can bind the final evidence
to the reported commit. Neither run changes matrix, workflow, support, or
release status.

## Approved archive replay

This later lane begins only after an exact ARM64 archive and checksum have been
accepted for owner-device review. Do not rebuild that approved input during
this replay.

Unpack into a new, owner-controlled directory outside the workspace. In a
fresh PowerShell process with no inherited application runtime variables:

```powershell
$bundle = Resolve-Path 'C:\approved\migration-stream-local-windows-arm64'
$workspace = 'C:\approved-workspaces\runtime-arm64-proof'
& "$bundle\migration-stream-local.exe" verify
& "$bundle\libexec\migration-stream-local-runtime.exe" verify
& "$bundle\migration-stream-local.exe" start --workspace $workspace --port 4173
& "$bundle\migration-stream-local.exe" status --workspace $workspace --port 4173
& "$bundle\migration-stream-local.exe" restart --workspace $workspace --port 4173
& "$bundle\migration-stream-local.exe" status --workspace $workspace --port 4173
& "$bundle\migration-stream-local.exe" stop --workspace $workspace --port 4173
& "$bundle\migration-stream-local.exe" status --workspace $workspace --port 4173
& "$bundle\migration-stream-local.exe" integrity --workspace $workspace --port 4173
& "$bundle\migration-stream-local.exe" status --workspace $workspace --port 4173
```

Before running commands, replay the exact checksum sidecar and manifest/archive
identity. Capture the versioned JSON output for every command and the exact
host values. The proof must show the same workspace filesystem, governed Object
Workspace, bundle, and application identities across start/restart and the
post-restart `OWNED` status while both owned processes are replaced. The
authenticated stop must then report `STOPPED`; prove port 4173 has no listener,
both replacement process handles have exited, protected lifecycle records are
absent, and the stopped private-session inventory is exact. Replay `STOPPED`,
capture bounded workspace/session digests, run integrity only while stopped,
and replay `STOPPED` again with the listener absent, the session still stopped,
and both digests unchanged. Only then may the foreign-listener proof run; that
listener must survive rejected start, restart, and stop commands without
workspace or private-session mutation.

Do not alter the target matrix, publish an ARM64 artifact, or call the target
supported based on a successful cross-build or this unreviewed local run.
