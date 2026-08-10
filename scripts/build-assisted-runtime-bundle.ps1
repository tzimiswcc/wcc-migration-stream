[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Output,
    [switch]$AllowUncommittedSnapshot,
    [switch]$NoArchive,
    [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')][string]$Target,
    [string]$RetainedBuildRoot,
    [string]$AssemblerResultFile
)

# Build-time only. The unpacked Windows runtime has no build-tool dependency.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not ('MigrationStreamBundleBuildDirectoryIdentity' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class MigrationStreamBundleBuildDirectoryIdentity
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(
        string name,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandleEx(
        SafeFileHandle file,
        int informationClass,
        IntPtr information,
        uint bufferSize);

    public static string Describe(string path)
    {
        const uint FileReadAttributes = 0x80;
        const uint FileShareRead = 0x1;
        const uint FileShareWrite = 0x2;
        const uint FileShareDelete = 0x4;
        const uint OpenExisting = 3;
        const uint FileFlagBackupSemantics = 0x02000000;
        const uint FileFlagOpenReparsePoint = 0x00200000;
        const int FileIdInfo = 18;
        const uint FileIdInfoSize = 24;

        using (SafeFileHandle handle = CreateFileW(
            path,
            FileReadAttributes,
            FileShareRead | FileShareWrite | FileShareDelete,
            IntPtr.Zero,
            OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero))
        {
            if (handle.IsInvalid)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "directory identity open failed");
            }
            IntPtr buffer = Marshal.AllocHGlobal((int)FileIdInfoSize);
            try
            {
                if (!GetFileInformationByHandleEx(handle, FileIdInfo, buffer, FileIdInfoSize))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "directory FileId query failed");
                }
                ulong volume = unchecked((ulong)Marshal.ReadInt64(buffer, 0));
                byte[] fileId = new byte[16];
                Marshal.Copy(IntPtr.Add(buffer, 8), fileId, 0, fileId.Length);
                return volume.ToString("x16") + ":" + BitConverter.ToString(fileId).Replace("-", "").ToLowerInvariant();
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }
    }
}
'@
}

function Get-PrivateDirectoryAuthority([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'private build authority requires one real directory'
    }
    $security = [IO.FileSystemAclExtensions]::GetAccessControl([IO.DirectoryInfo]$item.FullName)
    return [pscustomobject]@{
        Path = $item.FullName
        Identity = [MigrationStreamBundleBuildDirectoryIdentity]::Describe($item.FullName)
        Dacl = $security.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
    }
}

function Copy-PrivateReleaseBinaries {
    param(
        [Parameter(Mandatory = $true)][string]$SourceRoot,
        [Parameter(Mandatory = $true)][string]$SourceBoundaryRoot,
        [Parameter(Mandatory = $true)][string]$DestinationRoot
    )

    $sourceRootItem = Get-Item -LiteralPath $SourceRoot -Force
    $sourceBoundary = [IO.Path]::GetFullPath($SourceBoundaryRoot)
    $sourceAncestor = $sourceRootItem
    $sourceDepth = 0
    while ($true) {
        if (
            -not ($sourceAncestor.Attributes -band [IO.FileAttributes]::Directory) -or
            ($sourceAncestor.Attributes -band [IO.FileAttributes]::ReparsePoint)
        ) {
            throw 'Cargo release directory chain must contain only real directories'
        }
        if ([string]::Equals($sourceAncestor.FullName, $sourceBoundary, [StringComparison]::OrdinalIgnoreCase)) {
            break
        }
        $sourceDepth++
        if ($sourceDepth -gt 8 -or $null -eq $sourceAncestor.Parent) {
            throw 'Cargo release root escaped its exact target directory boundary'
        }
        $sourceAncestor = $sourceAncestor.Parent
    }
    $sourceRoot = $sourceRootItem.FullName
    if (Test-Path -LiteralPath $DestinationRoot) { throw 'private release binary staging root already exists' }
    $destinationParentItem = Get-Item -LiteralPath (Split-Path -Parent $DestinationRoot) -Force
    if ($destinationParentItem.PSIsContainer -eq $false -or ($destinationParentItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'private release binary staging parent must not be a reparse point'
    }
    New-Item -ItemType Directory -Path $DestinationRoot | Out-Null
    $destinationRoot = (Resolve-Path -LiteralPath $DestinationRoot).Path
    if ((Get-Item -LiteralPath $destinationRoot -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'private release binary staging root must not be a reparse point'
    }

    [long]$maximumMemberBytes = 128MB
    [long]$maximumTotalBytes = 512MB
    [long]$totalBytes = 0
    foreach ($name in @('migration-stream-local-runtime.exe', 'migration-stream.exe', 'migration-stream-workbench.exe', 'migration-stream-bundle.exe')) {
        $source = Join-Path $sourceRoot $name
        $destination = Join-Path $destinationRoot $name
        $sourceItem = Get-Item -LiteralPath $source -Force
        if (-not $sourceItem -or $sourceItem.PSIsContainer -or ($sourceItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Cargo output is not one real regular file: $name"
        }
        [long]$sourceLength = $sourceItem.Length
        if ($sourceLength -gt $maximumMemberBytes -or $totalBytes -gt ($maximumTotalBytes - $sourceLength)) {
            throw "Cargo output exceeds the staged byte bounds: $name"
        }
        $input = [IO.File]::Open($source, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            if ($input.Length -ne $sourceLength -or $input.Length -gt $maximumMemberBytes) {
                throw "Cargo output changed while opening: $name"
            }
            $output = [IO.File]::Open($destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $buffer = New-Object byte[] 65536
                [long]$observed = 0
                while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                    $observed += $read
                    if ($observed -gt $maximumMemberBytes -or $totalBytes -gt ($maximumTotalBytes - $observed)) {
                        throw "Cargo output exceeds the staged byte bounds: $name"
                    }
                    $output.Write($buffer, 0, $read)
                }
                $output.Flush($true)
                if ($observed -ne $sourceLength -or $output.Length -ne $observed) {
                    throw "Cargo output changed while staging: $name"
                }
            } finally { $output.Dispose() }
            $sourceAfterItem = Get-Item -LiteralPath $source -Force
            if ($sourceAfterItem.Length -ne $sourceLength -or $sourceAfterItem.PSIsContainer -or ($sourceAfterItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw "Cargo output changed while staging: $name"
            }
            $sourceHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash.ToLowerInvariant()
            $destinationHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash.ToLowerInvariant()
            if ($sourceHash -ne $destinationHash) { throw "Cargo output changed while staging: $name" }
            $destinationItem = Get-Item -LiteralPath $destination -Force
            if ($destinationItem.PSIsContainer -or ($destinationItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw "staged release binary identity is unsafe: $name"
            }
            if ($destinationItem.Length -ne $sourceLength) { throw "staged release binary length changed: $name" }
            $totalBytes += $observed
        } finally { $input.Dispose() }
    }
    return $destinationRoot
}

function Resolve-ConcreteGitExecutable {
    $command = Get-Command git -CommandType Application -ErrorAction Stop | Select-Object -First 1
    if (-not $command.Path -or -not [IO.Path]::IsPathFullyQualified($command.Path)) {
        throw 'Git must resolve to one concrete absolute application path'
    }

    $resolvedItem = Get-Item -LiteralPath $command.Path -Force
    $resolvedIsRealExecutable = (
        -not $resolvedItem.PSIsContainer -and
        -not ($resolvedItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -and
        $resolvedItem.Extension -ieq '.exe'
    )
    if ($resolvedIsRealExecutable -and $resolvedItem.LinkType -ine 'HardLink') {
        return $resolvedItem.FullName
    }

    # Git for Windows may expose cmd\git.exe as a package-manager hard link.
    # The assembler retains every executable path component and therefore uses
    # the byte-identical, single-link bin\git.exe authority when it exists.
    $candidates = [System.Collections.Generic.List[string]]::new()
    if (
        $resolvedIsRealExecutable -and
        $resolvedItem.LinkType -ieq 'HardLink' -and
        (Split-Path -Leaf (Split-Path -Parent $command.Path)) -ieq 'cmd'
    ) {
        $gitRoot = Split-Path -Parent (Split-Path -Parent $command.Path)
        $candidates.Add((Join-Path $gitRoot 'bin\git.exe'))
    }
    foreach ($candidate in $candidates | Select-Object -Unique) {
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { continue }
        $item = Get-Item -LiteralPath $candidate -Force
        if (
            -not $item.PSIsContainer -and
            -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -and
            $item.LinkType -ine 'HardLink' -and
            $item.Extension -ieq '.exe' -and
            $item.Length -eq $resolvedItem.Length -and
            (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash -ceq
                (Get-FileHash -LiteralPath $resolvedItem.FullName -Algorithm SHA256).Hash
        ) {
            return $item.FullName
        }
    }
    throw 'Git must resolve to one real git.exe file, not an alias, function, directory, or reparse point'
}

function Assert-ExactPeMachine {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][uint16]$ExpectedMachine,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -lt 256) { throw "$Label is not one bounded PE32+ executable" }
        $reader = [IO.BinaryReader]::new($stream, [Text.Encoding]::ASCII, $true)
        try {
            if ($reader.ReadUInt16() -ne 0x5A4D) { throw "$Label lacks the exact MZ signature" }
            $stream.Position = 0x3C
            [uint32]$peOffset = $reader.ReadUInt32()
            if ($peOffset -gt 1MB -or ([uint64]$peOffset + 26) -gt [uint64]$stream.Length) {
                throw "$Label has an out-of-bounds PE header"
            }
            $stream.Position = $peOffset
            if ($reader.ReadUInt32() -ne 0x00004550) { throw "$Label lacks the exact PE signature" }
            [uint16]$machine = $reader.ReadUInt16()
            $stream.Position = [uint64]$peOffset + 24
            [uint16]$optionalMagic = $reader.ReadUInt16()
            if ($machine -ne $ExpectedMachine -or $optionalMagic -ne 0x020B) {
                throw "$Label is not the exact requested PE32+ machine"
            }
        } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
}

function Assert-PrivateDirectoryAuthority([object]$Expected, [string]$Label) {
    $current = Get-PrivateDirectoryAuthority $Expected.Path
    if (
        -not [string]::Equals($current.Path, $Expected.Path, [StringComparison]::OrdinalIgnoreCase) -or
        $current.Identity -cne $Expected.Identity -or
        $current.Dacl -cne $Expected.Dacl
    ) {
        throw "$Label differs from its retained FileId or DACL authority"
    }
}

function Remove-HarnessOwnedBuildRoot(
    [string]$BuildRoot,
    [string]$ExpectedName,
    [object]$ExpectedRootAuthority,
    [object]$ExpectedParentAuthority
) {
    if (-not (Test-Path -LiteralPath $BuildRoot)) { return }
    Assert-PrivateDirectoryAuthority $ExpectedParentAuthority 'private build parent'
    Assert-PrivateDirectoryAuthority $ExpectedRootAuthority 'private build root'
    $item = Get-Item -LiteralPath $BuildRoot -Force
    if (
        -not $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $item.Name -cne $ExpectedName -or
        -not [string]::Equals($item.Parent.FullName, $ExpectedParentAuthority.Path, [StringComparison]::OrdinalIgnoreCase)
    ) {
        throw 'private build root identity is unsafe for cleanup'
    }
    $entries = @(Get-ChildItem -LiteralPath $BuildRoot -Force -Recurse)
    if (
        $entries.Count -gt 4096 -or
        @($entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count -ne 0
    ) {
        throw 'private build root cleanup graph is unsafe'
    }
    Assert-PrivateDirectoryAuthority $ExpectedParentAuthority 'private build parent before cleanup'
    Assert-PrivateDirectoryAuthority $ExpectedRootAuthority 'private build root before cleanup'
    Remove-Item -LiteralPath $BuildRoot -Recurse -Force
    if (Test-Path -LiteralPath $BuildRoot) { throw 'private build root cleanup was not proven' }
}

function Assert-RetainedBuildRoot(
    [string]$BuildRoot,
    [string]$ExpectedName,
    [object]$ExpectedRootAuthority,
    [object]$ExpectedParentAuthority
) {
    Assert-PrivateDirectoryAuthority $ExpectedParentAuthority 'retained private build parent'
    Assert-PrivateDirectoryAuthority $ExpectedRootAuthority 'retained private build root'
    $item = Get-Item -LiteralPath $BuildRoot -Force
    if (
        -not $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $item.Name -cne $ExpectedName -or
        -not [string]::Equals($item.Parent.FullName, $ExpectedParentAuthority.Path, [StringComparison]::OrdinalIgnoreCase)
    ) {
        throw 'retained private build root differs from its exact harness boundary'
    }
    $entries = @(Get-ChildItem -LiteralPath $BuildRoot -Force -Recurse)
    if (
        $entries.Count -gt 4096 -or
        @($entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count -ne 0
    ) {
        throw 'retained private build root graph is unsafe'
    }
}

if (-not [IO.Path]::IsPathFullyQualified($Output)) { throw '--Output must be absolute' }
if (-not $Target) {
    $Target = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        'aarch64-pc-windows-msvc'
    } elseif ($env:PROCESSOR_ARCHITECTURE -eq 'AMD64') {
        'x86_64-pc-windows-msvc'
    } else { throw 'host is not one native Windows portable-bundle target' }
}
$targetContract = if ($Target -eq 'aarch64-pc-windows-msvc') {
    [pscustomobject]@{ Processor = 'ARM64'; Runtime = 'Arm64'; Artifact = 'migration-stream-local-windows-arm64'; PeMachine = [uint16]0xAA64 }
} else {
    [pscustomobject]@{ Processor = 'AMD64'; Runtime = 'X64'; Artifact = 'migration-stream-local-windows-x64'; PeMachine = [uint16]0x8664 }
}
if (
    $env:PROCESSOR_ARCHITECTURE -cne $targetContract.Processor -or
    [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() -cne $targetContract.Runtime -or
    [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString() -cne $targetContract.Runtime
) {
    throw "Windows bundle assembly is native-only; host/process architecture does not match $Target"
}
$canonicalArtifact = $targetContract.Artifact
if ((Split-Path -Leaf $Output) -ne $canonicalArtifact) { throw "-Output basename must be the canonical artifact name: $canonicalArtifact" }
$candidateEnabled = $env:MIGRATION_STREAM_ENABLE_WINDOWS_NATIVE_ACCEPTANCE_CANDIDATE -eq '1'
$legacyX64Enabled = $Target -eq 'x86_64-pc-windows-msvc' -and $env:MIGRATION_STREAM_ENABLE_WINDOWS_X64_ACCEPTANCE_CANDIDATE -eq '1'
if (-not $candidateEnabled -and -not $legacyX64Enabled) {
    throw 'Windows bundle assembly is disabled outside the explicit native acceptance-candidate lane.'
}
$gitExecutable = Resolve-ConcreteGitExecutable
foreach ($command in @('python', 'npm', 'cargo', 'rustc')) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) { throw "missing build-time prerequisite: $command" }
}
$rustHostLines = @(& rustc -vV | Where-Object { $_ -clike 'host: *' })
if ($LASTEXITCODE -ne 0 -or $rustHostLines.Count -ne 1 -or $rustHostLines[0] -cne "host: $Target") {
    throw "Rust host is not the exact native Windows bundle target: $Target"
}
if (Test-Path -LiteralPath $Output) { throw 'bundle output already exists' }
$archive = [IO.Path]::ChangeExtension($Output, '.zip')
if ((Test-Path -LiteralPath $archive) -or (Test-Path -LiteralPath "$archive.sha256")) { throw 'bundle archive output already exists' }
if ($AssemblerResultFile) {
    if (-not [IO.Path]::IsPathFullyQualified($AssemblerResultFile)) {
        throw '-AssemblerResultFile must be absolute'
    }
    $assemblerResultFullPath = [IO.Path]::GetFullPath($AssemblerResultFile)
    foreach ($publishedOutput in @($Output, $archive, "$archive.sha256")) {
        if ([string]::Equals(
            $assemblerResultFullPath,
            [IO.Path]::GetFullPath($publishedOutput),
            [StringComparison]::OrdinalIgnoreCase
        )) {
            throw '-AssemblerResultFile must not collide with a bundle publication path'
        }
    }
    if (Test-Path -LiteralPath $AssemblerResultFile) {
        throw 'assembler result output already exists'
    }
    $assemblerResultParent = Get-Item -LiteralPath (Split-Path -Parent $AssemblerResultFile) -Force
    if (
        -not $assemblerResultParent.PSIsContainer -or
        ($assemblerResultParent.Attributes -band [IO.FileAttributes]::ReparsePoint)
    ) {
        throw 'assembler result parent must be one real directory'
    }
}
if ($RetainedBuildRoot) {
    if (-not [IO.Path]::IsPathFullyQualified($RetainedBuildRoot)) {
        throw '-RetainedBuildRoot must be absolute'
    }
    $retainedBuildRootFullPath = [IO.Path]::GetFullPath($RetainedBuildRoot)
    $retainedBuildRootParentPath = [IO.Path]::GetFullPath((Split-Path -Parent $retainedBuildRootFullPath))
    $outputParentPath = [IO.Path]::GetFullPath((Split-Path -Parent $Output))
    $repositoryPath = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($root))
    $repositoryBoundary = $repositoryPath + [IO.Path]::DirectorySeparatorChar
    if (
        [IO.Path]::GetFileName($retainedBuildRootFullPath) -cne 'migration-stream-private-bundle-build' -or
        -not [string]::Equals($retainedBuildRootParentPath, $outputParentPath, [StringComparison]::OrdinalIgnoreCase) -or
        [string]::Equals($retainedBuildRootFullPath, $repositoryPath, [StringComparison]::OrdinalIgnoreCase) -or
        $retainedBuildRootFullPath.StartsWith($repositoryBoundary, [StringComparison]::OrdinalIgnoreCase) -or
        (Test-Path -LiteralPath $retainedBuildRootFullPath)
    ) {
        throw '-RetainedBuildRoot must be one fresh fixed child of the bundle output parent outside the repository'
    }
    $retainedBuildRootParent = Get-Item -LiteralPath $retainedBuildRootParentPath -Force
    if (
        -not $retainedBuildRootParent.PSIsContainer -or
        ($retainedBuildRootParent.Attributes -band [IO.FileAttributes]::ReparsePoint)
    ) {
        throw '-RetainedBuildRoot parent must be one real directory'
    }
    if ($AssemblerResultFile) {
        $retainedBoundary = [IO.Path]::TrimEndingDirectorySeparator($retainedBuildRootFullPath) +
            [IO.Path]::DirectorySeparatorChar
        if ([IO.Path]::GetFullPath($AssemblerResultFile).StartsWith($retainedBoundary, [StringComparison]::OrdinalIgnoreCase)) {
            throw '-AssemblerResultFile must remain outside the retained private build root'
        }
    }
}
$provenanceEnvironmentNames = @(
    'MIGRATION_STREAM_PRODUCT_COMMIT',
    'MIGRATION_STREAM_SOURCE_SNAPSHOT_SHA256',
    'MIGRATION_STREAM_SOURCE_STATE',
    'MIGRATION_STREAM_FRONTEND_ASSETS_SHA256'
)
$provenanceEnvironmentBefore = @{}
foreach ($name in $provenanceEnvironmentNames) {
    $provenanceEnvironmentBefore[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
Push-Location $root
try {
    & $gitExecutable diff --cached --quiet
    if ($LASTEXITCODE -ne 0) { throw 'staged changes are ambiguous; use one clean source' }
    $status = (& $gitExecutable status --porcelain=v1 --untracked-files=all) -join "`n"
    if ($status -and -not $AllowUncommittedSnapshot) { throw 'source is dirty; use -AllowUncommittedSnapshot only for an explicit local proof' }
    $commit = (& $gitExecutable rev-parse --verify HEAD).Trim()
    $sourceState = if ($status) { 'explicit-uncommitted-snapshot' } else { 'clean' }
    if ($RetainedBuildRoot) {
        $buildRootName = 'migration-stream-private-bundle-build'
        $buildRoot = [IO.Path]::GetFullPath($RetainedBuildRoot)
    } else {
        $buildRootName = "migration-stream-bundle-build-$([Guid]::NewGuid())"
        $buildRoot = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetTempPath()) $buildRootName))
    }
    $buildRootParentAuthority = Get-PrivateDirectoryAuthority (Split-Path -Parent $buildRoot)
    $assemblerText = $null
    New-Item -ItemType Directory -Path $buildRoot | Out-Null
    Assert-PrivateDirectoryAuthority $buildRootParentAuthority 'private build parent after creation'
    $buildRootAuthority = Get-PrivateDirectoryAuthority $buildRoot
    try {
        $sourceManifest = Join-Path $buildRoot 'source-manifest.json'
        $sourceSha = (& python scripts/runtime-source-snapshot.py $root $sourceManifest $gitExecutable).Trim()
        if ($LASTEXITCODE -ne 0) { throw 'source snapshot failed' }
        & npm --prefix web ci --offline --no-audit --no-fund
        if ($LASTEXITCODE -ne 0) { throw 'offline frontend dependency install failed' }
        & npm --prefix web run typecheck
        if ($LASTEXITCODE -ne 0) { throw 'frontend typecheck failed' }
        $assets = Join-Path $buildRoot 'frontend-assets'
        Push-Location web
        try { & .\node_modules\.bin\vite.cmd build --outDir $assets --emptyOutDir; if ($LASTEXITCODE -ne 0) { throw 'frontend build failed' } } finally { Pop-Location }
        # The Rust assembler independently verifies the frontend graph; this
        # build script supplies the deterministic staging hash through a small
        # canonical helper retained in the existing POSIX build path for now.
        $frontendManifest = Join-Path $buildRoot 'frontend-manifest.json'
        $frontendSha = (& python scripts/runtime-frontend-snapshot.py $assets $frontendManifest).Trim()
        if ($LASTEXITCODE -ne 0) { throw 'frontend snapshot failed' }
        $env:MIGRATION_STREAM_PRODUCT_COMMIT = $commit
        $env:MIGRATION_STREAM_SOURCE_SNAPSHOT_SHA256 = $sourceSha
        $env:MIGRATION_STREAM_SOURCE_STATE = $sourceState
        $env:MIGRATION_STREAM_FRONTEND_ASSETS_SHA256 = $frontendSha
        $targetDirectory = Join-Path $root 'target'
        if (-not (Test-Path -LiteralPath $targetDirectory)) {
            $repositoryItem = Get-Item -LiteralPath $root -Force
            if (-not $repositoryItem.PSIsContainer -or ($repositoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw 'repository root is not one real directory'
            }
            New-Item -ItemType Directory -Path $targetDirectory | Out-Null
        }
        $targetDirectoryItem = Get-Item -LiteralPath $targetDirectory -Force
        if (
            -not $targetDirectoryItem.PSIsContainer -or
            ($targetDirectoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            -not [string]::Equals($targetDirectoryItem.Parent.FullName, $root, [StringComparison]::OrdinalIgnoreCase)
        ) {
            throw 'Cargo target directory is not one exact real repository child'
        }
        $targetTripleDirectory = Join-Path $targetDirectory $Target
        $releaseDirectory = Join-Path $targetTripleDirectory 'release'
        $expectedParent = $targetDirectoryItem
        foreach ($buildDirectory in @($targetTripleDirectory, $releaseDirectory)) {
            if (-not (Test-Path -LiteralPath $buildDirectory)) {
                New-Item -ItemType Directory -Path $buildDirectory | Out-Null
            }
            $buildDirectoryItem = Get-Item -LiteralPath $buildDirectory -Force
            if (
                -not $buildDirectoryItem.PSIsContainer -or
                ($buildDirectoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                -not [string]::Equals(
                    $buildDirectoryItem.Parent.FullName,
                    $expectedParent.FullName,
                    [StringComparison]::OrdinalIgnoreCase
                )
            ) {
                throw 'Cargo target architecture chain is not one exact real directory path'
            }
            $expectedParent = $buildDirectoryItem
        }
        $cargoArguments = @('build', '--locked', '--release', '--bins', '--target', $Target, '--target-dir', $targetDirectory, '--features', 'windows-native-acceptance-candidate')
        & cargo @cargoArguments
        if ($LASTEXITCODE -ne 0) { throw 'release binary build failed' }
        $binaries = Copy-PrivateReleaseBinaries `
            -SourceRoot $releaseDirectory `
            -SourceBoundaryRoot $targetDirectory `
            -DestinationRoot (Join-Path $buildRoot 'release-binaries')
        foreach ($name in @('migration-stream-local-runtime.exe', 'migration-stream.exe', 'migration-stream-workbench.exe', 'migration-stream-bundle.exe')) {
            Assert-ExactPeMachine -Path (Join-Path $binaries $name) -ExpectedMachine $targetContract.PeMachine -Label $name
        }
        $assemblerArguments = @('--repository-root', $root, '--git-executable', $gitExecutable, '--frontend-assets-root', $assets, '--release-binaries-root', $binaries, '--output-directory', $Output, '--product-commit', $commit, '--source-state', $sourceState, '--source-snapshot-sha256', $sourceSha, '--frontend-assets-sha256', $frontendSha)
        if (-not $NoArchive) { $assemblerArguments += '--create-zip-archive' }
        $assemblerOutput = @(& (Join-Path $binaries 'migration-stream-bundle.exe') @assemblerArguments)
        $assemblerExitCode = $LASTEXITCODE
        if ($assemblerExitCode -ne 0) { throw 'bundle assembler failed' }
        $assemblerText = $assemblerOutput -join "`n"
        try {
            $assemblerDocument = $assemblerText | ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw 'bundle assembler success output is not one JSON document'
        }
        if (
            $assemblerDocument.schemaVersion -isnot [string] -or
            $assemblerDocument.schemaVersion -cne 'migration-stream/assisted-local-runtime/assembly/v1' -or
            $assemblerDocument.ok -isnot [bool] -or
            $assemblerDocument.ok -ne $true -or
            $assemblerDocument.result.manifestSha256 -isnot [string] -or
            $assemblerDocument.result.manifestSha256 -cnotmatch '^[0-9a-f]{64}$' -or
            [uint64]$assemblerDocument.result.memberCount -eq 0 -or
            [uint64]$assemblerDocument.result.totalMemberBytes -eq 0 -or
            [IO.Path]::GetFullPath([string]$assemblerDocument.result.bundleDirectory) -cne [IO.Path]::GetFullPath($Output)
        ) {
            throw 'bundle assembler success envelope differs from the exact contract'
        }
        if ($NoArchive) {
            if (
                $null -ne $assemblerDocument.result.archivePath -or
                $null -ne $assemblerDocument.result.archiveSha256 -or
                $null -ne $assemblerDocument.result.archiveChecksumPath
            ) {
                throw 'bundle assembler unexpectedly published archive outputs'
            }
        } elseif (
            $assemblerDocument.result.archiveSha256 -isnot [string] -or
            $assemblerDocument.result.archiveSha256 -cnotmatch '^[0-9a-f]{64}$' -or
            [IO.Path]::GetFullPath([string]$assemblerDocument.result.archivePath) -cne [IO.Path]::GetFullPath($archive) -or
            [IO.Path]::GetFullPath([string]$assemblerDocument.result.archiveChecksumPath) -cne [IO.Path]::GetFullPath("$archive.sha256")
        ) {
            throw 'bundle assembler archive envelope differs from the exact contract'
        }
    } finally {
        if ($RetainedBuildRoot) {
            Assert-RetainedBuildRoot `
                $buildRoot `
                $buildRootName `
                $buildRootAuthority `
                $buildRootParentAuthority
        } else {
            Remove-HarnessOwnedBuildRoot `
                $buildRoot `
                $buildRootName `
                $buildRootAuthority `
                $buildRootParentAuthority
        }
    }
    if ($AssemblerResultFile) {
        $resultBytes = [Text.UTF8Encoding]::new($false).GetBytes($assemblerText)
        $resultPartial = Join-Path `
            (Split-Path -Parent $AssemblerResultFile) `
            (".$([IO.Path]::GetFileName($AssemblerResultFile)).retained-partial-$([Guid]::NewGuid())")
        $resultStream = [IO.File]::Open(
            $resultPartial,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        try {
            $resultStream.Write($resultBytes, 0, $resultBytes.Length)
            $resultStream.Flush($true)
        } finally { $resultStream.Dispose() }
        [IO.File]::Move($resultPartial, $AssemblerResultFile)
    } else {
        Write-Output $assemblerText
    }
} finally {
    foreach ($name in $provenanceEnvironmentNames) {
        [Environment]::SetEnvironmentVariable($name, $provenanceEnvironmentBefore[$name], 'Process')
    }
    Pop-Location
}
