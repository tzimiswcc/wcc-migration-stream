[CmdletBinding()]
param(
    [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')]
    [string]$Target = 'x86_64-pc-windows-msvc',
    [ValidateRange(0, 65535)][int]$Port = 0,
    [string]$EvidenceDirectory,
    [string]$EvidenceFileName,
    [switch]$AllowUncommittedSnapshot
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$targetContract = if ($Target -eq 'aarch64-pc-windows-msvc') {
    [pscustomobject]@{
        Artifact = 'migration-stream-local-windows-arm64'
        Architecture = 'arm64'
        Processor = 'ARM64'
        Runtime = 'Arm64'
        Node = 'arm64'
        PeMachine = [uint16]0xAA64
        PeMachineLabel = '0xAA64'
    }
} else {
    [pscustomobject]@{
        Artifact = 'migration-stream-local-windows-x64'
        Architecture = 'x64'
        Processor = 'AMD64'
        Runtime = 'X64'
        Node = 'x64'
        PeMachine = [uint16]0x8664
        PeMachineLabel = '0x8664'
    }
}
$tempRoot = [IO.Path]::TrimEndingDirectorySeparator(
    (Resolve-Path -LiteralPath ([IO.Path]::GetTempPath())).Path
)
$tempRootItem = Get-Item -LiteralPath $tempRoot -Force
if (-not $tempRootItem.PSIsContainer -or ($tempRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'system temporary root is not one real directory'
}
if (-not $EvidenceDirectory) { $EvidenceDirectory = $tempRoot }
if (-not [IO.Path]::IsPathFullyQualified($EvidenceDirectory)) { throw '-EvidenceDirectory must be absolute' }
$evidenceRoot = (Resolve-Path -LiteralPath $EvidenceDirectory).Path
$evidenceRootItem = Get-Item -LiteralPath $evidenceRoot -Force
if (-not $evidenceRootItem.PSIsContainer -or ($evidenceRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'evidence root is not one real directory'
}
$repositoryBoundary = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($root)) + [IO.Path]::DirectorySeparatorChar
$evidenceBoundary = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($evidenceRoot))
if (
    [string]::Equals($evidenceBoundary, [IO.Path]::TrimEndingDirectorySeparator($root), [StringComparison]::OrdinalIgnoreCase) -or
    $evidenceBoundary.StartsWith($repositoryBoundary, [StringComparison]::OrdinalIgnoreCase)
) {
    throw '-EvidenceDirectory must remain outside the repository source boundary'
}
$acceptanceId = [Guid]::NewGuid().ToString()
$workName = "migration-stream-windows-$($targetContract.Architecture)-acceptance-$acceptanceId"
$work = Join-Path $tempRoot $workName
$artifact = Join-Path $work $targetContract.Artifact
$workspace = Join-Path $work 'fresh-workspace'
$port = $Port
$assemblerResultFile = Join-Path $work 'assembler-result.private.json'
$retainedBuildRoot = Join-Path $work 'migration-stream-private-bundle-build'
if (-not $EvidenceFileName) {
    $EvidenceFileName = "windows-$($targetContract.Architecture)-native-acceptance-$acceptanceId.json"
}
if (
    $EvidenceFileName -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.json$' -or
    [IO.Path]::GetFileName($EvidenceFileName) -cne $EvidenceFileName
) {
    throw '-EvidenceFileName must be one bounded JSON filename component'
}
$summary = Join-Path $evidenceRoot $EvidenceFileName
$summaryStream = $null
$succeeded = $false
$lifecycleSucceeded = $false
$projectRuntimeBridgeOk = $false
$bundleEntrypointsVerified = $false
$lifecycleStartAttempted = $false
$commandSequence = 0
$completedStages = [System.Collections.Generic.List[string]]::new()
$foreignListener = $null
$foreignPort = 0
$manifestSha256 = $null
$productCommit = $null
$sourceState = $null
$sourceSnapshotSha256 = $null
$archiveSha256 = $null
$archiveEntryCount = 0
$bundleMemberCount = 0
$totalMemberBytes = 0
$releaseExecutableCount = 0
$shippedExecutableCount = 0
$checksumSidecarMatched = $false
$assemblerArchiveReplayOk = $false
$assembledManifestReplayOk = $false
$unpackedManifestReplayOk = $false
$rootVerifyOk = $false
$controllerVerifyOk = $false
$allReleaseExecutablesNative = $false
$allShippedExecutablesNative = $false
$launcherControllerByteIdentical = $false
$builderPrivateRootRetained = $false
$workspaceInitialized = $false
$liveIdentityChangedAfterInitialize = $false
$idempotentIdentityPreserved = $false
$restartIdentityPreserved = $false
$restartProcessesReplaced = $false
$integrityOk = $false
$mainListenerStopped = $false
$foreignListenerPreserved = $false
$foreignStateUnchanged = $false
$lifecycleAuthorityContinuous = $false
$cleanupAttempted = $false
$authenticatedLifecycleStopOk = $false
$boundedCleanupStopOk = $false
$ownedProcessesExited = $false
$protectedRecordsAbsent = $false
$persistentLocksReplayed = $false
$mainListenerAbsent = $false
$foreignListenerReleased = $false
$foreignListenerCreated = $false
$workRootRemoved = $false
$workRootDisposition = 'NOT_CREATED'
$workRootIdentitySha256 = $null
$tempRootAuthorityFact = $null
$workRootAuthorityFact = $null
$runtimeResourcesOk = $false
$cleanupOk = $false
$processCleanupUnproven = $false
$ownedProcessHandleProofIncomplete = $false
$observedClassifications = [System.Collections.Generic.List[object]]::new()
$rejections = [System.Collections.Generic.List[object]]::new()
$retainedOwnedProcesses = [System.Collections.Generic.List[Diagnostics.Process]]::new()
$preRestartSupervisor = $null
$preRestartWorkbench = $null
$replacementSupervisor = $null
$replacementWorkbench = $null
$sessionDirectory = $null
$workspaceFilesystemIdentitySha256 = $null
$objectWorkspaceDescriptorSha256 = $null
$cleanupArtifact = $null
$hostFacts = $null
$runtimeFailure = $null
$cleanupFailure = $null
$cleanupSafetyMode = $false
$assemblyResult = $null
$currentStage = 'script-initialization'
$acceptanceTimer = [System.Diagnostics.Stopwatch]::StartNew()
$acceptanceDeadline = $null
$runtimeCommandCleanupTimeoutMilliseconds = 15000
$runtimeCommandStreamTimeoutMilliseconds = 5000
$runtimeCommandCancellationDrainTimeoutMilliseconds = 2000
$runtimeCommandOutputBound = 128 * 1024
$allowedEvidenceStages = @(
    'script-initialization',
    'bundle-build',
    'assembled-inspection',
    'assembled-verify',
    'assembled-controller-verify',
    'archive-replay',
    'archive-unpack',
    'unpacked-inspection',
    'unpacked-verify',
    'unpacked-controller-verify',
    'start',
    'status',
    'controller-status',
    'object-workspace-get',
    'object-workspace-initialize',
    'object-workspace-active',
    'restart',
    'stop',
    'integrity',
    'foreign-listener',
    'start-rejected',
    'restart-rejected',
    'stop-rejected',
    'project-runtime-bridge',
    'cleanup-status',
    'cleanup',
    'complete'
)

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Win32.SafeHandles;

public static class MigrationStreamBoundedProcessOutput
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

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandle(
        SafeFileHandle file,
        out BY_HANDLE_FILE_INFORMATION information);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandleEx(
        SafeFileHandle file,
        int informationClass,
        IntPtr information,
        uint bufferSize);

    public static string DescribeFileSystemObject(string path)
    {
        const uint FileReadAttributes = 0x80;
        const uint FileShareRead = 0x1;
        const uint FileShareWrite = 0x2;
        const uint OpenExisting = 3;
        const uint FileFlagBackupSemantics = 0x02000000;
        const uint FileFlagOpenReparsePoint = 0x00200000;
        using (SafeFileHandle handle = CreateFileW(
            path,
            FileReadAttributes,
            FileShareRead | FileShareWrite,
            IntPtr.Zero,
            OpenExisting,
            FileFlagBackupSemantics | FileFlagOpenReparsePoint,
            IntPtr.Zero))
        {
            if (handle.IsInvalid)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            if (!GetFileInformationByHandle(handle, out BY_HANDLE_FILE_INFORMATION information))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            const int FileIdInfo = 18;
            const int FileIdInfoSize = 24;
            IntPtr fileIdBuffer = Marshal.AllocHGlobal(FileIdInfoSize);
            try
            {
                if (!GetFileInformationByHandleEx(
                    handle,
                    FileIdInfo,
                    fileIdBuffer,
                    FileIdInfoSize))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                }
                ulong volumeSerialNumber = unchecked((ulong)Marshal.ReadInt64(fileIdBuffer));
                byte[] identifier = new byte[16];
                Marshal.Copy(IntPtr.Add(fileIdBuffer, 8), identifier, 0, identifier.Length);
                string fileId = BitConverter.ToString(identifier).Replace("-", "").ToLowerInvariant();
                ulong creationTime =
                    ((ulong)(uint)information.CreationTime.dwHighDateTime << 32) |
                    (uint)information.CreationTime.dwLowDateTime;
                return string.Format(
                    "{0:x16}:{1}:{2}:{3:x8}:{4:x16}",
                    volumeSerialNumber,
                    fileId,
                    information.NumberOfLinks,
                    information.FileAttributes,
                    creationTime);
            }
            finally
            {
                Marshal.FreeHGlobal(fileIdBuffer);
            }
        }
    }

    public static async Task<byte[]> ReadAsync(
        Stream stream,
        int maximumBytes,
        CancellationToken cancellationToken)
    {
        byte[] buffer = new byte[8192];
        using (MemoryStream output = new MemoryStream())
        {
            while (true)
            {
                int read = await stream.ReadAsync(
                    buffer,
                    0,
                    buffer.Length,
                    cancellationToken).ConfigureAwait(false);
                if (read == 0)
                {
                    return output.ToArray();
                }
                if (output.Length > maximumBytes - read)
                {
                    throw new InvalidDataException("process output exceeded its bound");
                }
                output.Write(buffer, 0, read);
            }
        }
    }
}
'@

function Select-HarnessOwnedPort([int]$RequestedPort) {
    $reservation = [System.Net.Sockets.TcpListener]::new(
        [System.Net.IPAddress]::Loopback,
        $RequestedPort
    )
    try {
        $reservation.Start()
        return ([System.Net.IPEndPoint]$reservation.LocalEndpoint).Port
    } finally { $reservation.Stop() }
}

function Assert-ExactPeMachine([string]$Path, [uint16]$ExpectedMachine, [string]$Label) {
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

function Assert-BundleExecutableInventory([string]$BundleRoot) {
    $expected = @(
        'bin/migration-stream-workbench.exe',
        'bin/migration-stream.exe',
        'libexec/migration-stream-local-runtime.exe',
        'migration-stream-local.exe'
    ) | Sort-Object -CaseSensitive
    $actual = @(
        Get-ChildItem -LiteralPath $BundleRoot -Recurse -Force -File -Filter '*.exe' |
            ForEach-Object {
                [IO.Path]::GetRelativePath($BundleRoot, $_.FullName).Replace('\', '/')
            } |
            Sort-Object -CaseSensitive
    )
    if ($actual.Count -ne $expected.Count -or (($actual -join "`n") -cne ($expected -join "`n"))) {
        throw 'shipped Windows executable inventory differs from the exact bundle contract'
    }
    foreach ($relative in $actual) {
        Assert-ExactPeMachine `
            (Join-Path $BundleRoot $relative) `
            $targetContract.PeMachine `
            $relative
    }
    $launcherHash = (Get-FileHash -LiteralPath (Join-Path $BundleRoot 'migration-stream-local.exe') -Algorithm SHA256).Hash
    $controllerHash = (Get-FileHash -LiteralPath (Join-Path $BundleRoot 'libexec/migration-stream-local-runtime.exe') -Algorithm SHA256).Hash
    if ($launcherHash -cne $controllerHash) {
        throw 'Windows launcher is not byte-identical to the native controller'
    }
    return $actual.Count
}

function Read-BundleManifestEvidence([string]$BundleRoot) {
    $manifestPath = Join-Path $BundleRoot 'bundle-manifest.json'
    $manifestItem = Get-Item -LiteralPath $manifestPath -Force
    if (
        $manifestItem.PSIsContainer -or
        ($manifestItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $manifestItem.Length -gt 1MB
    ) {
        throw 'bundle manifest is not one bounded real file'
    }
    $manifest = [IO.File]::ReadAllText($manifestPath, [Text.UTF8Encoding]::new($false, $true)) |
        ConvertFrom-Json -ErrorAction Stop
    if (
        $manifest.target.os -cne 'windows' -or
        $manifest.target.architecture -cne $targetContract.Architecture -or
        $manifest.target.targetTriple -cne $Target -or
        $manifest.members.Count -le 0 -or
        $manifest.members.Count -gt 1024
    ) {
        throw 'bundle manifest target or member contract differs'
    }
    [uint64]$total = 0
    foreach ($member in $manifest.members) {
        [uint64]$size = $member.size
        if ($size -gt 128MB -or $total -gt (512MB - $size)) {
            throw 'bundle manifest exceeds its member or total byte bound'
        }
        $total += $size
    }
    return [pscustomobject]@{
        Document = $manifest
        ManifestSha256 = [string]$manifest.manifestSha256
        ProductCommit = [string]$manifest.product.commit
        SourceState = [string]$manifest.product.sourceState
        SourceSnapshotSha256 = [string]$manifest.product.sourceSnapshotSha256
        MemberCount = [int]$manifest.members.Count
        TotalMemberBytes = $total
        RawFileLength = [uint64]$manifestItem.Length
        RawFileSha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Read-AssemblerResultEvidence([string]$Path, [string]$ExpectedBundleRoot) {
    $item = Get-Item -LiteralPath $Path -Force
    if (
        $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $item.Length -gt 64KB
    ) {
        throw 'assembler result is not one bounded real file'
    }
    $document = [IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false, $true)) |
        ConvertFrom-Json -ErrorAction Stop
    if (
        $document.schemaVersion -isnot [string] -or
        $document.schemaVersion -cne 'migration-stream/assisted-local-runtime/assembly/v1' -or
        $document.ok -isnot [bool] -or
        $document.ok -ne $true -or
        $document.result.manifestSha256 -isnot [string] -or
        $document.result.manifestSha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $document.result.archiveSha256 -isnot [string] -or
        $document.result.archiveSha256 -cnotmatch '^[0-9a-f]{64}$' -or
        [uint64]$document.result.memberCount -eq 0 -or
        [uint64]$document.result.totalMemberBytes -eq 0 -or
        [IO.Path]::GetFullPath([string]$document.result.bundleDirectory) -cne [IO.Path]::GetFullPath($ExpectedBundleRoot) -or
        [IO.Path]::GetFullPath([string]$document.result.archivePath) -cne [IO.Path]::GetFullPath("$ExpectedBundleRoot.zip") -or
        [IO.Path]::GetFullPath([string]$document.result.archiveChecksumPath) -cne [IO.Path]::GetFullPath("$ExpectedBundleRoot.zip.sha256")
    ) {
        throw 'assembler result differs from the exact published bundle contract'
    }
    return $document.result
}

function Expand-VerifiedBundleArchive(
    [string]$ArchivePath,
    [string]$DestinationRoot,
    [object]$ManifestEvidence
) {
    $archiveItem = Get-Item -LiteralPath $ArchivePath -Force
    if (
        $archiveItem.PSIsContainer -or
        ($archiveItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $archiveItem.Length -gt 512MB
    ) {
        throw 'bundle archive is not one bounded real file'
    }
    $destinationItem = Get-Item -LiteralPath $DestinationRoot -Force
    if (
        -not $destinationItem.PSIsContainer -or
        ($destinationItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        @(Get-ChildItem -LiteralPath $DestinationRoot -Force).Count -ne 0
    ) {
        throw 'archive destination is not one fresh real directory'
    }

    $expectedFiles = [System.Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    $expectedDirectories = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $rootEntry = "$($targetContract.Artifact)/"
    $null = $expectedDirectories.Add($rootEntry)
    $expectedFiles.Add(
        "$($targetContract.Artifact)/bundle-manifest.json",
        [pscustomobject]@{
            Length = [uint64]$ManifestEvidence.RawFileLength
            Sha256 = [string]$ManifestEvidence.RawFileSha256
            ExternalAttributes = [Convert]::ToUInt32('81240000', 16)
        }
    )
    foreach ($member in $ManifestEvidence.Document.members) {
        $relative = [string]$member.path
        if (
            [string]::IsNullOrWhiteSpace($relative) -or
            $relative.StartsWith('/', [StringComparison]::Ordinal) -or
            $relative.EndsWith('/', [StringComparison]::Ordinal) -or
            $relative -cmatch '[\\:]' -or
            $relative -cmatch '[\x00-\x1f\x7f]'
        ) {
            throw 'manifest contains an unsafe archive member name'
        }
        $parts = $relative.Split('/')
        if (@($parts | Where-Object { $_ -eq '' -or $_ -eq '.' -or $_ -eq '..' }).Count -ne 0) {
            throw 'manifest contains a noncanonical archive member component'
        }
        $mode = [string]$member.mode
        $externalAttributes = switch ($mode) {
            '0444' { [Convert]::ToUInt32('81240000', 16) }
            '0555' { [Convert]::ToUInt32('816D0000', 16) }
            default { throw 'manifest contains an unsupported archive member mode' }
        }
        $sha256 = [string]$member.sha256
        Assert-LowerHex $sha256 64 'manifest member SHA-256'
        $expectedName = "$($targetContract.Artifact)/$relative"
        if ($expectedFiles.ContainsKey($expectedName)) { throw 'manifest archive member is duplicated' }
        $expectedFiles.Add(
            $expectedName,
            [pscustomobject]@{
                Length = [uint64]$member.size
                Sha256 = $sha256
                ExternalAttributes = $externalAttributes
            }
        )
        for ($index = 0; $index -lt ($parts.Count - 1); $index++) {
            $directory = "$($targetContract.Artifact)/$(($parts[0..$index]) -join '/')/"
            $null = $expectedDirectories.Add($directory)
        }
    }

    $archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        if ($archive.Entries.Count -ne ($expectedFiles.Count + $expectedDirectories.Count)) {
            throw 'archive entry count differs from its exact manifest graph'
        }
        $expectedOrder = [System.Collections.Generic.List[string]]::new()
        $null = $expectedOrder.Add($rootEntry)
        $null = $expectedOrder.Add("$($targetContract.Artifact)/bundle-manifest.json")
        [string[]]$sortedDirectories = @(
            $expectedDirectories | Where-Object { $_ -cne $rootEntry }
        )
        [Array]::Sort($sortedDirectories, [StringComparer]::Ordinal)
        foreach ($directory in $sortedDirectories) { $null = $expectedOrder.Add($directory) }
        foreach ($member in $ManifestEvidence.Document.members) {
            $null = $expectedOrder.Add("$($targetContract.Artifact)/$([string]$member.path)")
        }
        for ($index = 0; $index -lt $archive.Entries.Count; $index++) {
            if ($archive.Entries[$index].FullName -cne $expectedOrder[$index]) {
                throw 'archive entry order differs from the deterministic manifest graph'
            }
        }
        $seen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($entry in $archive.Entries) {
            $name = $entry.FullName
            if (
                -not $seen.Add($name) -or
                $name -cmatch '[\\:]' -or
                $name -cmatch '[\x00-\x1f\x7f]' -or
                -not $name.StartsWith($rootEntry, [StringComparison]::Ordinal)
            ) {
                throw 'archive contains an unsafe or duplicate entry name'
            }
            $externalAttributes = [BitConverter]::ToUInt32(
                [BitConverter]::GetBytes([int]$entry.ExternalAttributes),
                0
            )
            if ($expectedDirectories.Contains($name)) {
                if (
                    -not $name.EndsWith('/', [StringComparison]::Ordinal) -or
                    $entry.Length -ne 0 -or
                    $externalAttributes -ne [Convert]::ToUInt32('416D0000', 16)
                ) {
                    throw 'archive directory entry differs from the exact deterministic contract'
                }
            } elseif ($expectedFiles.ContainsKey($name)) {
                $expectedFile = $expectedFiles[$name]
                if (
                    $name.EndsWith('/', [StringComparison]::Ordinal) -or
                    [uint64]$entry.Length -ne [uint64]$expectedFile.Length -or
                    $externalAttributes -ne [uint32]$expectedFile.ExternalAttributes
                ) {
                    throw 'archive file entry differs from the exact deterministic contract'
                }
            } else {
                throw 'archive contains material outside its exact manifest graph'
            }
            if (
                $entry.LastWriteTime.Year -ne 1980 -or
                $entry.LastWriteTime.Month -ne 1 -or
                $entry.LastWriteTime.Day -ne 1 -or
                $entry.LastWriteTime.TimeOfDay -ne [TimeSpan]::Zero
            ) {
                throw 'archive entry timestamp differs from the deterministic contract'
            }
        }

        $destinationBoundary = [IO.Path]::TrimEndingDirectorySeparator(
            [IO.Path]::GetFullPath($DestinationRoot)
        ) + [IO.Path]::DirectorySeparatorChar
        foreach ($directoryName in @($expectedDirectories | Sort-Object Length)) {
            $relativeDirectory = $directoryName.TrimEnd('/').Replace('/', [IO.Path]::DirectorySeparatorChar)
            $directoryPath = [IO.Path]::GetFullPath((Join-Path $DestinationRoot $relativeDirectory))
            if (-not $directoryPath.StartsWith($destinationBoundary, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'archive directory escaped its fresh extraction root'
            }
            $created = [IO.Directory]::CreateDirectory($directoryPath)
            if ($created.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw 'archive extraction created or traversed a reparse directory'
            }
        }
        foreach ($entry in $archive.Entries) {
            if (-not $expectedFiles.ContainsKey($entry.FullName)) { continue }
            $expectedFile = $expectedFiles[$entry.FullName]
            $relativeFile = $entry.FullName.Replace('/', [IO.Path]::DirectorySeparatorChar)
            $filePath = [IO.Path]::GetFullPath((Join-Path $DestinationRoot $relativeFile))
            if (-not $filePath.StartsWith($destinationBoundary, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'archive file escaped its fresh extraction root'
            }
            $parent = Get-Item -LiteralPath (Split-Path -Parent $filePath) -Force
            if (-not $parent.PSIsContainer -or ($parent.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw 'archive file parent is not one real directory'
            }
            $input = $entry.Open()
            $output = [IO.File]::Open($filePath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            $hasher = [Security.Cryptography.IncrementalHash]::CreateHash(
                [Security.Cryptography.HashAlgorithmName]::SHA256
            )
            try {
                $buffer = New-Object byte[] 65536
                [uint64]$observed = 0
                while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                    $observed += [uint64]$read
                    if ($observed -gt [uint64]$expectedFile.Length) {
                        throw 'archive member exceeded its manifest byte bound during extraction'
                    }
                    $hasher.AppendData($buffer, 0, $read)
                    $output.Write($buffer, 0, $read)
                }
                $output.Flush($true)
                $observedSha256 = [Convert]::ToHexString($hasher.GetHashAndReset()).ToLowerInvariant()
                if (
                    $observed -ne [uint64]$expectedFile.Length -or
                    $output.Length -ne [long]$observed -or
                    $observedSha256 -cne [string]$expectedFile.Sha256
                ) {
                    throw 'archive member bytes differ from the exact assembled manifest graph'
                }
            } finally {
                $hasher.Dispose()
                $output.Dispose()
                $input.Dispose()
            }
        }
        return $archive.Entries.Count
    } finally { $archive.Dispose() }
}

function Get-WindowsObjectAuthorityFact([IO.FileSystemInfo]$Item) {
    if ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'bounded tree authority fact refused a reparse point'
    }
    $identity = [MigrationStreamBoundedProcessOutput]::DescribeFileSystemObject($Item.FullName)
    $security = if ($Item.PSIsContainer) {
        [IO.FileSystemAclExtensions]::GetAccessControl([IO.DirectoryInfo]$Item)
    } else {
        [IO.FileSystemAclExtensions]::GetAccessControl([IO.FileInfo]$Item)
    }
    $sddl = $security.GetSecurityDescriptorSddlForm(
        [Security.AccessControl.AccessControlSections]::All
    )
    return "$identity|$sddl"
}

function Get-BoundedTreeDigest(
    [string]$TreeRoot,
    [int]$MaximumEntries = 1024,
    [uint64]$MaximumMemberBytes = 128MB,
    [uint64]$MaximumTotalBytes = 512MB
) {
    if (-not (Test-Path -LiteralPath $TreeRoot)) {
        return [Convert]::ToHexString(
            [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes('ABSENT'))
        ).ToLowerInvariant()
    }
    $rootItem = Get-Item -LiteralPath $TreeRoot -Force
    if (-not $rootItem.PSIsContainer -or ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'bounded tree root is not one real directory'
    }
    $entries = @(Get-ChildItem -LiteralPath $TreeRoot -Force -Recurse)
    if ($MaximumEntries -le 0 -or $entries.Count -gt $MaximumEntries) {
        throw 'bounded tree entry count exceeded'
    }
    [uint64]$total = 0
    $facts = [System.Collections.Generic.List[string]]::new()
    $facts.Add("R|$(Get-WindowsObjectAuthorityFact $rootItem)")
    foreach ($entry in $entries) {
        if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw 'bounded tree contains a reparse point'
        }
        $relative = [IO.Path]::GetRelativePath($TreeRoot, $entry.FullName).Replace('\', '/')
        $authority = Get-WindowsObjectAuthorityFact $entry
        if ($entry.PSIsContainer) {
            $facts.Add("D|$relative|$authority")
            continue
        }
        [uint64]$length = $entry.Length
        if ($length -gt $MaximumMemberBytes -or $total -gt ($MaximumTotalBytes - $length)) {
            throw 'bounded tree byte count exceeded'
        }
        $total += $length
        $hash = (Get-FileHash -LiteralPath $entry.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $facts.Add("F|$relative|$length|$hash|$authority")
    }
    $facts.Sort([StringComparer]::Ordinal)
    $bytes = [Text.Encoding]::UTF8.GetBytes(($facts -join "`n"))
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
}

function Assert-StoppedSessionState([string]$SessionRoot) {
    $sessionItem = Get-Item -LiteralPath $SessionRoot -Force
    if (
        -not $sessionItem.PSIsContainer -or
        ($sessionItem.Attributes -band [IO.FileAttributes]::ReparsePoint)
    ) {
        throw 'stopped private session root is not one real directory'
    }
    foreach ($component in @('launch-request-json', 'session-json')) {
        if (Test-Path -LiteralPath (Join-Path $SessionRoot $component)) {
            throw 'protected Windows runtime state remains after authenticated stop'
        }
    }
    $sessionTemp = Join-Path $SessionRoot 'session-temp'
    if ((Test-Path -LiteralPath $sessionTemp) -and @(Get-ChildItem -LiteralPath $sessionTemp -Force).Count -ne 0) {
        throw 'Windows runtime temporary directory is not empty after authenticated stop'
    }
    $expected = @('command-lock', 'owner-lock', 'session-temp') | Sort-Object -CaseSensitive
    $actual = @(Get-ChildItem -LiteralPath $SessionRoot -Force | ForEach-Object Name | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $expected.Count -or ($actual -join "`n") -cne ($expected -join "`n")) {
        throw 'stopped private session inventory differs from the exact persistent-lock contract'
    }
    foreach ($component in @('command-lock', 'owner-lock')) {
        $lock = Get-Item -LiteralPath (Join-Path $SessionRoot $component) -Force
        if ($lock.PSIsContainer -or ($lock.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'persistent runtime lock is not one real file'
        }
    }
    $tempItem = Get-Item -LiteralPath $sessionTemp -Force
    if (-not $tempItem.PSIsContainer -or ($tempItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'persistent runtime session-temp is not one real directory'
    }
}

function Assert-PrivateSessionDirectory([string]$SessionRoot) {
    if (-not [IO.Path]::IsPathFullyQualified($SessionRoot)) {
        throw 'lifecycle result lacks one absolute private session directory'
    }
    $sessionItem = Get-Item -LiteralPath $SessionRoot -Force
    $localAppData = [Environment]::GetFolderPath(
        [Environment+SpecialFolder]::LocalApplicationData,
        [Environment+SpecialFolderOption]::DoNotVerify
    )
    $expectedRuntimeRoot = [IO.Path]::GetFullPath((Join-Path $localAppData 'migration-stream-runtime'))
    $expectedRuntimeRootItem = Get-Item -LiteralPath $expectedRuntimeRoot -Force
    $reportedRuntimeRootItem = Get-Item -LiteralPath $sessionItem.Parent.FullName -Force
    if (
        -not $sessionItem.PSIsContainer -or
        ($sessionItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $sessionItem.Name -cnotmatch '^session-[0-9a-f]{24}$' -or
        -not $expectedRuntimeRootItem.PSIsContainer -or
        ($expectedRuntimeRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        -not $reportedRuntimeRootItem.PSIsContainer -or
        ($reportedRuntimeRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        [MigrationStreamBoundedProcessOutput]::DescribeFileSystemObject($expectedRuntimeRootItem.FullName) -cne
            [MigrationStreamBoundedProcessOutput]::DescribeFileSystemObject($reportedRuntimeRootItem.FullName)
    ) {
        throw 'lifecycle private session differs from the exact local runtime authority'
    }
}

function Get-RetainedProcess([object]$RawPid, [string]$Label) {
    [int]$processId = $RawPid
    if ($processId -le 0) { throw "$Label PID is absent" }
    $process = [Diagnostics.Process]::GetProcessById($processId)
    try { $null = $process.Handle } catch { $process.Dispose(); throw "$Label process handle was not retained" }
    return $process
}

function Retain-ReportedOwnedProcesses([object]$Result, [string]$Label) {
    $classification = [string]$Result.classification
    $supervisor = $null
    $workbench = $null
    if ($null -ne $Result.supervisorPid) {
        try {
            $supervisor = Get-RetainedProcess $Result.supervisorPid "$Label supervisor"
        } catch {
            $script:ownedProcessHandleProofIncomplete = $true
            $script:processCleanupUnproven = $true
            throw
        }
        $script:retainedOwnedProcesses.Add($supervisor) | Out-Null
    }
    if ($null -ne $Result.workbenchPid) {
        try {
            $workbench = Get-RetainedProcess $Result.workbenchPid "$Label workbench"
        } catch {
            $script:ownedProcessHandleProofIncomplete = $true
            $script:processCleanupUnproven = $true
            throw
        }
        $script:retainedOwnedProcesses.Add($workbench) | Out-Null
    }
    if (
        $classification -ceq 'OWNED' -and
        ($null -eq $supervisor -or $null -eq $workbench)
    ) {
        $script:ownedProcessHandleProofIncomplete = $true
        $script:processCleanupUnproven = $true
        throw "$Label OWNED authority did not expose both retainable processes"
    }
    if (
        $classification -cin @('STARTING', 'STOPPING') -and
        $null -eq $supervisor -and
        $null -eq $workbench
    ) {
        $script:ownedProcessHandleProofIncomplete = $true
        $script:processCleanupUnproven = $true
        throw "$Label transitional authority did not expose a retainable process"
    }
    return [pscustomobject]@{
        Supervisor = $supervisor
        Workbench = $workbench
    }
}

function Assert-ProcessExited([Diagnostics.Process]$Process, [string]$Label) {
    if (-not $Process.WaitForExit(15000)) { throw "$Label did not exit within the cleanup bound" }
}

function Assert-ListenerConnectable([int]$ListenerPort) {
    $probe = [System.Net.Sockets.TcpClient]::new()
    try {
        $connect = $probe.ConnectAsync([System.Net.IPAddress]::Loopback, $ListenerPort)
        if (-not $connect.Wait(3000) -or -not $probe.Connected) {
            throw 'harness-owned foreign listener is not connectable'
        }
    } finally { $probe.Dispose() }
}

function Assert-LowerHex([object]$Value, [int]$Length, [string]$Label) {
    $text = [string]$Value
    if ($text -cnotmatch "^[0-9a-f]{$Length}$") { throw "$Label is not exact lowercase hexadecimal" }
}

function Get-RuntimePort([string[]]$Arguments) {
    for ($index = 0; $index -lt ($Arguments.Count - 1); $index++) {
        if ($Arguments[$index] -eq '--port') { return [int]$Arguments[$index + 1] }
    }
    throw 'runtime acceptance stage lacks an explicit port'
}

function Get-FixedFailureMessage([string]$Message) {
    if ($Message -match '(?i)authenticated runtime readiness') {
        return 'authenticated runtime readiness was not reached'
    }
    if ($Message -match '(?i)runtime supervisor.*exited|supervisor exited') {
        return 'runtime supervisor exited before authenticated readiness'
    }
    return 'runtime CLI failure details suppressed'
}

function Set-RuntimeFailureEvidence([string]$Stage, [int]$ExitCode, [object]$Document, [long]$ElapsedMilliseconds) {
    if ($null -ne $script:runtimeFailure) { return }
    if ($script:allowedEvidenceStages -cnotcontains $Stage) {
        throw 'acceptance harness tried to record a non-allowlisted runtime stage'
    }
    $code = 'RUNTIME_COMMAND_INVALID_ENVELOPE'
    $message = 'runtime CLI failure details suppressed'
    if (
        $ExitCode -ne 0 -and
        $null -ne $Document -and
        $Document.schemaVersion -is [string] -and
        $Document.schemaVersion -ceq 'migration-stream/assisted-local-runtime/cli/v1' -and
        $Document.ok -is [bool] -and
        $Document.ok -eq $false -and
        $Document.code -is [string] -and
        $Document.code -cmatch '^[A-Z][A-Z0-9_]{0,95}$' -and
        $Document.message -is [string] -and
        -not [string]::IsNullOrWhiteSpace($Document.message)
    ) {
        $code = [string]$Document.code
        $message = Get-FixedFailureMessage ([string]$Document.message)
    }
    $script:runtimeFailure = [ordered]@{
        stage = $Stage
        code = $code
        message = $message
        elapsedMilliseconds = $ElapsedMilliseconds
    }
    Publish-AcceptanceEvidence
}

function Set-HarnessFailureEvidence([string]$Stage, [string]$Code, [string]$Message, [long]$ElapsedMilliseconds) {
    if ($null -ne $script:runtimeFailure) { return }
    if ($script:allowedEvidenceStages -cnotcontains $Stage) {
        throw 'acceptance harness tried to record a non-allowlisted failure stage'
    }
    if ($Code -cnotmatch '^[A-Z][A-Z0-9_]{0,95}$') {
        throw 'acceptance harness tried to record an invalid failure code'
    }
    $script:runtimeFailure = [ordered]@{
        stage = $Stage
        code = $Code
        message = $Message
        elapsedMilliseconds = $ElapsedMilliseconds
    }
    Publish-AcceptanceEvidence
}

function Set-CleanupFailureEvidence([string]$Code, [string]$Message) {
    if ($null -ne $script:cleanupFailure) { return }
    if ($Code -cnotmatch '^[A-Z][A-Z0-9_]{0,95}$') {
        throw 'acceptance harness tried to record an invalid cleanup failure code'
    }
    $script:cleanupFailure = [ordered]@{
        stage = 'cleanup'
        code = $Code
        message = $Message
        elapsedMilliseconds = $script:acceptanceTimer.ElapsedMilliseconds
    }
    Publish-AcceptanceEvidence
}

function Write-AcceptanceEvidence {
    $evidence = [ordered]@{
        schemaVersion = 'migration-stream/windows-native-acceptance-evidence/v1'
        evidenceBoundary = 'ACCEPTANCE_CANDIDATE_ONLY'
        ok = $script:succeeded
        target = [ordered]@{
            artifact = $script:targetContract.Artifact
            targetTriple = $script:Target
            architecture = $script:targetContract.Architecture
            expectedPeMachine = $script:targetContract.PeMachineLabel
        }
        host = $script:hostFacts
        source = [ordered]@{
            productCommit = $script:productCommit
            sourceState = $script:sourceState
            sourceSnapshotSha256 = $script:sourceSnapshotSha256
        }
        bundle = [ordered]@{
            manifestSha256 = $script:manifestSha256
            archiveSha256 = $script:archiveSha256
            memberCount = $script:bundleMemberCount
            totalMemberBytes = $script:totalMemberBytes
            archiveEntryCount = $script:archiveEntryCount
            releaseExecutableCount = $script:releaseExecutableCount
            shippedExecutableCount = $script:shippedExecutableCount
            checksumSidecarMatched = $script:checksumSidecarMatched
            assemblerArchiveReplayOk = $script:assemblerArchiveReplayOk
            assembledManifestReplayOk = $script:assembledManifestReplayOk
            unpackedManifestReplayOk = $script:unpackedManifestReplayOk
            rootVerifyOk = $script:rootVerifyOk
            controllerVerifyOk = $script:controllerVerifyOk
            allReleaseExecutablesNative = $script:allReleaseExecutablesNative
            allShippedExecutablesNative = $script:allShippedExecutablesNative
            launcherControllerByteIdentical = $script:launcherControllerByteIdentical
            builderPrivateRootRetained = $script:builderPrivateRootRetained
        }
        statusSchemaVersion = 'migration-stream/assisted-local-runtime/status/v2'
        integritySchemaVersion = 'migration-stream/assisted-local-runtime/integrity/v2'
        lifecycle = [ordered]@{
            observedClassifications = @($script:observedClassifications)
            rejections = @($script:rejections)
            workspaceInitialized = $script:workspaceInitialized
            liveIdentityChangedAfterInitialize = $script:liveIdentityChangedAfterInitialize
            idempotentIdentityPreserved = $script:idempotentIdentityPreserved
            restartIdentityPreserved = $script:restartIdentityPreserved
            restartProcessesReplaced = $script:restartProcessesReplaced
            integrityOk = $script:integrityOk
            mainListenerStopped = $script:mainListenerStopped
            foreignListenerPreserved = $script:foreignListenerPreserved
            foreignStateUnchanged = $script:foreignStateUnchanged
            lifecycleAuthorityContinuous = $script:lifecycleAuthorityContinuous
            authenticatedStopOk = $script:authenticatedLifecycleStopOk
            projectRuntimeBridgeOk = $script:projectRuntimeBridgeOk
        }
        cleanup = [ordered]@{
            cleanupAttempted = $script:cleanupAttempted
            boundedStopOk = $script:boundedCleanupStopOk
            ownedProcessesExited = $script:ownedProcessesExited
            protectedRecordsAbsent = $script:protectedRecordsAbsent
            persistentLocksReplayed = $script:persistentLocksReplayed
            mainListenerAbsent = $script:mainListenerAbsent
            processCleanupUnproven = $script:processCleanupUnproven
            foreignListenerReleased = $script:foreignListenerReleased
            workRootRemoved = $script:workRootRemoved
            workRootDisposition = $script:workRootDisposition
            workRootIdentitySha256 = $script:workRootIdentitySha256
            runtimeResourcesOk = $script:runtimeResourcesOk
            ok = $script:cleanupOk
        }
        completedStages = @($script:completedStages)
        commandCount = $script:commandSequence
    }
    if ($null -ne $script:runtimeFailure) {
        $evidence['failure'] = $script:runtimeFailure
    } elseif (-not $script:succeeded) {
        $evidence['failure'] = [ordered]@{
            stage = $script:currentStage
            code = 'RUNTIME_ACCEPTANCE_INCOMPLETE'
            message = 'native acceptance did not reach a terminal result'
            elapsedMilliseconds = $script:acceptanceTimer.ElapsedMilliseconds
        }
    }
    if ($null -ne $script:cleanupFailure) {
        $evidence['cleanupFailure'] = $script:cleanupFailure
    }
    $json = $evidence | ConvertTo-Json -Depth 7 -Compress
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json)
    if ($null -eq $script:summaryStream) {
        $script:summaryStream = [IO.File]::Open(
            $script:summary,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::Read
        )
    }
    $script:summaryStream.Position = 0
    $script:summaryStream.SetLength(0)
    $script:summaryStream.Write($bytes, 0, $bytes.Length)
    $script:summaryStream.Flush($true)
}

function Publish-AcceptanceEvidence {
    try {
        Write-AcceptanceEvidence
    } catch {
        if (-not $script:cleanupSafetyMode) { throw }
        Write-Warning 'intermediate acceptance evidence update failed during safety cleanup'
    }
}

function Set-AcceptanceStage([string]$Stage) {
    if ($script:allowedEvidenceStages -cnotcontains $Stage) {
        throw 'acceptance harness selected a non-allowlisted evidence stage'
    }
    $script:currentStage = $Stage
    Publish-AcceptanceEvidence
    Write-Host "Windows acceptance stage started: $Stage"
}

function Complete-AcceptanceStage([string]$Stage) {
    if ($script:currentStage -cne $Stage) {
        throw 'acceptance harness completed a different evidence stage'
    }
    $script:completedStages.Add($Stage) | Out-Null
    Write-Host "Windows acceptance stage completed: $Stage"
    Publish-AcceptanceEvidence
}

function Get-RemainingAcceptanceMilliseconds {
    if ($null -eq $script:acceptanceDeadline) { return [int]::MaxValue }
    $remaining = [long][Math]::Floor(
        ($script:acceptanceDeadline - [DateTimeOffset]::UtcNow).TotalMilliseconds
    )
    if ($remaining -le 0) {
        Set-HarnessFailureEvidence `
            $script:currentStage `
            'RUNTIME_ACCEPTANCE_DEADLINE' `
            'native acceptance exceeded its post-build deadline' `
            $script:acceptanceTimer.ElapsedMilliseconds
        throw 'native acceptance exceeded its post-build deadline'
    }
    return [int][Math]::Min($remaining, [int]::MaxValue)
}

function Get-RuntimeCommandTimeoutMilliseconds([string]$Stage) {
    $stageMaximum = switch ($Stage) {
        'start' { 90000 }
        'restart' { 90000 }
        'integrity' { 120000 }
        'cleanup' { 60000 }
        default { 60000 }
    }
    return [Math]::Min($stageMaximum, (Get-RemainingAcceptanceMilliseconds))
}

function Get-HttpTimeoutSeconds {
    $remaining = Get-RemainingAcceptanceMilliseconds
    return [Math]::Max(1, [Math]::Min(15, [Math]::Floor($remaining / 1000)))
}

function Invoke-BoundedInheritedProcess(
    [string]$Stage,
    [string]$Executable,
    [string[]]$Arguments,
    [int]$TimeoutMilliseconds
) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    foreach ($argument in $Arguments) { $startInfo.ArgumentList.Add($argument) | Out-Null }

    $process = $null
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $process = [System.Diagnostics.Process]::Start($startInfo)
        if ($null -eq $process) { throw 'bounded acceptance process did not start' }
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            try {
                if (-not $process.HasExited) { $process.Kill($true) }
            } catch {
                if (-not $process.HasExited) {
                    $script:processCleanupUnproven = $true
                    Set-HarnessFailureEvidence $Stage 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN' 'timed-out process cleanup was not proven' $timer.ElapsedMilliseconds
                    throw 'bounded acceptance process cleanup was not proven'
                }
            }
            if (-not $process.WaitForExit($script:runtimeCommandCleanupTimeoutMilliseconds)) {
                $script:processCleanupUnproven = $true
                Set-HarnessFailureEvidence $Stage 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN' 'timed-out process cleanup was not proven' $timer.ElapsedMilliseconds
                throw 'bounded acceptance process cleanup was not proven'
            }
            Set-HarnessFailureEvidence $Stage 'RUNTIME_PROCESS_EXIT_TIMEOUT' 'acceptance process exceeded its stage deadline' $timer.ElapsedMilliseconds
            throw 'bounded acceptance process exceeded its stage deadline'
        }
        if ($process.ExitCode -ne 0) {
            Set-HarnessFailureEvidence $Stage 'RUNTIME_STAGE_FAILED' 'acceptance process reported failure' $timer.ElapsedMilliseconds
            throw 'bounded acceptance process reported failure'
        }
    } finally {
        $timer.Stop()
        if ($null -ne $process) { $process.Dispose() }
    }
}

function Invoke-NativeRuntimeProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [int]$TimeoutMilliseconds
) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $startInfo.ArgumentList.Add($argument) | Out-Null }

    $process = $null
    $failureCode = $null
    $failureMessage = $null
    $stdout = ''
    $stderr = ''
    $exitCode = -1
    $readCancellation = [System.Threading.CancellationTokenSource]::new()
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $process = [System.Diagnostics.Process]::Start($startInfo)
        if ($null -eq $process) { throw 'runtime process did not start' }
        $stdoutTask = [MigrationStreamBoundedProcessOutput]::ReadAsync(
            $process.StandardOutput.BaseStream,
            $script:runtimeCommandOutputBound,
            $readCancellation.Token
        )
        $stderrTask = [MigrationStreamBoundedProcessOutput]::ReadAsync(
            $process.StandardError.BaseStream,
            $script:runtimeCommandOutputBound,
            $readCancellation.Token
        )
        if (-not $process.WaitForExit($TimeoutMilliseconds)) {
            $failureCode = 'RUNTIME_PROCESS_EXIT_TIMEOUT'
            $failureMessage = 'runtime command exceeded its stage deadline'
            try {
                if (-not $process.HasExited) { $process.Kill($true) }
            } catch {
                if (-not $process.HasExited) {
                    throw 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN'
                }
            }
            if (-not $process.WaitForExit($script:runtimeCommandCleanupTimeoutMilliseconds)) {
                throw 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN'
            }
        }
        $streamTasks = [System.Threading.Tasks.Task[]]@($stdoutTask, $stderrTask)
        try {
            $streamsClosed = [System.Threading.Tasks.Task]::WaitAll(
                $streamTasks,
                $script:runtimeCommandStreamTimeoutMilliseconds
            )
        } catch [System.AggregateException] {
            $streamsClosed = $true
            $readerExceptions = $_.Exception.Flatten().InnerExceptions
            $outputExceeded = @(
                $readerExceptions | Where-Object { $_ -is [System.IO.InvalidDataException] }
            ).Count -ne 0
            if ($null -eq $failureCode) {
                if ($outputExceeded) {
                    $failureCode = 'RUNTIME_OUTPUT_BOUND_EXCEEDED'
                    $failureMessage = 'runtime command output exceeded its evidence bound'
                } else {
                    $failureCode = 'RUNTIME_OUTPUT_READ_FAILED'
                    $failureMessage = 'runtime command output could not be read'
                }
            }
        }
        if (-not $streamsClosed) {
            if ($null -eq $failureCode) {
                $failureCode = 'RUNTIME_STDIO_DRAIN_TIMEOUT'
                $failureMessage = 'runtime command streams did not reach EOF after process exit'
            }
            $readCancellation.Cancel()
            $process.StandardOutput.Dispose()
            $process.StandardError.Dispose()
            try {
                $readsSettled = [System.Threading.Tasks.Task]::WaitAll(
                    $streamTasks,
                    $script:runtimeCommandCancellationDrainTimeoutMilliseconds
                )
            } catch [System.AggregateException] {
                $readsSettled = $true
            }
            if (-not $readsSettled) {
                $failureCode = 'RUNTIME_STDIO_CLEANUP_UNPROVEN'
                $failureMessage = 'runtime command stream cleanup was not proven'
            }
        } elseif ($null -eq $failureCode) {
            try {
                $utf8 = [System.Text.UTF8Encoding]::new($false, $true)
                $stdout = $utf8.GetString($stdoutTask.GetAwaiter().GetResult())
                $stderr = $utf8.GetString($stderrTask.GetAwaiter().GetResult())
            } catch {
                $failureCode = 'RUNTIME_OUTPUT_INVALID_UTF8'
                $failureMessage = 'runtime command output was not strict UTF-8'
            }
        }
        $exitCode = $process.ExitCode
    } finally {
        $timer.Stop()
        $readCancellation.Cancel()
        $readCancellation.Dispose()
        if ($null -ne $process) { $process.Dispose() }
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        StandardOutput = $stdout
        StandardError = $stderr
        ElapsedMilliseconds = $timer.ElapsedMilliseconds
        FailureCode = $failureCode
        FailureMessage = $failureMessage
    }
}

function Invoke-Runtime(
    [string[]]$Arguments,
    [string]$ExpectedClassification = '',
    [switch]$Controller,
    [string]$StagePrefix = '',
    [string]$ArtifactRoot = '',
    [int]$TimeoutMilliseconds = 0
) {
    $script:commandSequence++
    $operation = $Arguments[0]
    $stage = if ($Controller) { "controller-$operation" } else { $operation }
    if ($StagePrefix) { $stage = "$StagePrefix-$stage" }
    $runtimeArtifact = if ($ArtifactRoot) { $ArtifactRoot } else { $artifact }
    $executable = if ($Controller) { Join-Path $runtimeArtifact 'libexec/migration-stream-local-runtime.exe' } else { Join-Path $runtimeArtifact 'migration-stream-local.exe' }
    Set-AcceptanceStage $stage
    try {
        $commandTimeoutMilliseconds = if ($TimeoutMilliseconds -gt 0) {
            if ($TimeoutMilliseconds -gt 120000) { throw 'runtime timeout override exceeds its fixed bound' }
            $TimeoutMilliseconds
        } else {
            Get-RuntimeCommandTimeoutMilliseconds $stage
        }
        $invocation = Invoke-NativeRuntimeProcess `
            $executable `
            $Arguments `
            $commandTimeoutMilliseconds
    } catch {
        if ($_.Exception.Message -ceq 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN') {
            $script:processCleanupUnproven = $true
            Set-HarnessFailureEvidence $stage 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN' 'runtime process cleanup was not proven' 0
        } else {
            Set-HarnessFailureEvidence $stage 'RUNTIME_PROCESS_START_FAILED' 'runtime command could not be started' 0
        }
        throw "runtime acceptance stage could not execute: $stage"
    }
    if ($null -ne $invocation.FailureCode) {
        Set-HarnessFailureEvidence $stage $invocation.FailureCode $invocation.FailureMessage $invocation.ElapsedMilliseconds
        throw "runtime acceptance stage did not reach a valid process boundary: $stage"
    }
    $documentText = if ($invocation.ExitCode -eq 0) { $invocation.StandardOutput } else { $invocation.StandardError }
    try { $document = $documentText | ConvertFrom-Json -ErrorAction Stop } catch {
        Set-RuntimeFailureEvidence $stage $invocation.ExitCode $null $invocation.ElapsedMilliseconds
        throw "runtime acceptance stage did not emit JSON: $stage"
    }
    if ($invocation.ExitCode -ne 0) {
        Set-RuntimeFailureEvidence $stage $invocation.ExitCode $document $invocation.ElapsedMilliseconds
        throw "runtime acceptance stage failed: $stage"
    }
    if ($document.ok -isnot [bool] -or $document.ok -ne $true) {
        Set-RuntimeFailureEvidence $stage $invocation.ExitCode $document $invocation.ElapsedMilliseconds
        throw "runtime acceptance stage lacks ok=true: $stage"
    }
    if ($document.schemaVersion -isnot [string] -or $document.schemaVersion -cne 'migration-stream/assisted-local-runtime/cli/v1') { throw "runtime acceptance stage has unexpected schemaVersion: $stage" }

    switch ($operation) {
        'verify' {
            Assert-LowerHex $document.result.manifestSha256 64 'verified manifest SHA-256'
            Assert-LowerHex $document.result.productCommit 40 'verified product commit'
            if ($script:manifestSha256 -and $document.result.manifestSha256 -ne $script:manifestSha256) { throw 'bundle entrypoints verified different manifests' }
            if ($script:productCommit -and $document.result.productCommit -ne $script:productCommit) { throw 'bundle entrypoints verified different product commits' }
            $script:manifestSha256 = [string]$document.result.manifestSha256
            $script:productCommit = [string]$document.result.productCommit
        }
        'integrity' {
            if ($document.result.schemaVersion -isnot [string] -or $document.result.schemaVersion -cne 'migration-stream/assisted-local-runtime/integrity/v2') { throw 'unexpected integrity schema' }
            if ($document.result.integrityResult.ok -isnot [bool] -or $document.result.integrityResult.ok -ne $true) { throw 'integrity result is not ok=true' }
            Assert-LowerHex $document.result.bundle.manifestSha256 64 'integrity manifest SHA-256'
            Assert-LowerHex $document.result.bundle.productCommit 40 'integrity product commit'
            Assert-LowerHex $document.result.workspace.filesystemIdentitySha256 64 'integrity workspace identity'
            if ($script:manifestSha256 -and $document.result.bundle.manifestSha256 -ne $script:manifestSha256) { throw 'integrity manifest differs from verified bundle' }
            if ($script:productCommit -and $document.result.bundle.productCommit -ne $script:productCommit) { throw 'integrity product commit differs from verified bundle' }
            if (
                $script:workspaceFilesystemIdentitySha256 -and
                [string]$document.result.workspace.filesystemIdentitySha256 -cne $script:workspaceFilesystemIdentitySha256
            ) {
                throw 'integrity workspace authority differs from the lifecycle baseline'
            }
            if ($script:objectWorkspaceDescriptorSha256 -and [string]$document.result.workspace.objectWorkspace.descriptorHash -cne $script:objectWorkspaceDescriptorSha256) {
                throw 'integrity Object Workspace authority differs from the lifecycle baseline'
            }
        }
        default {
            if ($document.result.schemaVersion -isnot [string] -or $document.result.schemaVersion -cne 'migration-stream/assisted-local-runtime/status/v2') { throw 'unexpected lifecycle status schema' }
            if (
                $ExpectedClassification -and
                (
                    $document.result.classification -isnot [string] -or
                    $document.result.classification -cne $ExpectedClassification
                )
            ) {
                throw 'runtime acceptance stage returned an unexpected lifecycle classification'
            }
            $runtimePort = Get-RuntimePort $Arguments
            if ($document.result.listenAddress -ne "127.0.0.1:$runtimePort") { throw 'unexpected lifecycle listen address' }
            if ($document.result.url -ne "http://127.0.0.1:$runtimePort") { throw 'unexpected lifecycle URL' }
            Assert-LowerHex $document.result.bundle.manifestSha256 64 'lifecycle manifest SHA-256'
            Assert-LowerHex $document.result.bundle.productCommit 40 'lifecycle product commit'
            Assert-LowerHex $document.result.workspace.filesystemIdentitySha256 64 'lifecycle workspace identity'
            if ($script:manifestSha256 -and $document.result.bundle.manifestSha256 -ne $script:manifestSha256) { throw 'lifecycle manifest differs from verified bundle' }
            if ($script:productCommit -and $document.result.bundle.productCommit -ne $script:productCommit) { throw 'lifecycle product commit differs from verified bundle' }
            if ($document.result.classification -ceq 'OWNED') {
                if ([uint64]$document.result.supervisorPid -eq 0 -or [uint64]$document.result.workbenchPid -eq 0) { throw 'owned lifecycle status lacks process identity' }
                Assert-LowerHex $document.result.applicationIdentitySha256 64 'owned application identity'
            }
            if ($script:workspaceFilesystemIdentitySha256) {
                $requiresNoProcesses = $document.result.classification -in @('STOPPED', 'FOREIGN_LISTENER')
                Assert-BaselineLifecycleAuthority $document.result -RequireNoProcesses:$requiresNoProcesses
            }
            $script:observedClassifications.Add([ordered]@{
                stage = $stage
                classification = [string]$document.result.classification
            }) | Out-Null
        }
    }
    Complete-AcceptanceStage $stage
    return $document
}

function Invoke-RuntimeFailure([string[]]$Arguments) {
    $script:commandSequence++
    $operation = $Arguments[0]
    $stage = "$operation-rejected"
    Set-AcceptanceStage $stage
    try {
        $invocation = Invoke-NativeRuntimeProcess `
            (Join-Path $artifact 'migration-stream-local.exe') `
            $Arguments `
            (Get-RuntimeCommandTimeoutMilliseconds $stage)
    } catch {
        if ($_.Exception.Message -ceq 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN') {
            $script:processCleanupUnproven = $true
            Set-HarnessFailureEvidence $stage 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN' 'runtime process cleanup was not proven' 0
        } else {
            Set-HarnessFailureEvidence $stage 'RUNTIME_PROCESS_START_FAILED' 'runtime command could not be started' 0
        }
        throw "runtime acceptance rejection stage could not execute: $stage"
    }
    if ($null -ne $invocation.FailureCode) {
        Set-HarnessFailureEvidence $stage $invocation.FailureCode $invocation.FailureMessage $invocation.ElapsedMilliseconds
        throw "runtime acceptance rejection stage did not reach a valid process boundary: $stage"
    }
    if ($invocation.ExitCode -eq 0) {
        Set-HarnessFailureEvidence $stage 'RUNTIME_EXPECTED_CONFLICT_MISSING' 'foreign-listener conflict was not enforced' $invocation.ElapsedMilliseconds
        throw "runtime acceptance rejection stage unexpectedly succeeded: $stage"
    }
    try {
        $document = $invocation.StandardError | ConvertFrom-Json -ErrorAction Stop
    } catch {
        Set-RuntimeFailureEvidence $stage $invocation.ExitCode $null $invocation.ElapsedMilliseconds
        throw "runtime acceptance rejection stage did not emit JSON: $stage"
    }
    if (
        $document.schemaVersion -isnot [string] -or
        $document.schemaVersion -cne 'migration-stream/assisted-local-runtime/cli/v1' -or
        $document.ok -isnot [bool] -or
        $document.ok -ne $false -or
        $document.code -isnot [string] -or
        $document.code -cne 'RUNTIME_CONFLICT' -or
        $document.message -isnot [string] -or
        [string]::IsNullOrWhiteSpace($document.message)
    ) {
        Set-RuntimeFailureEvidence $stage $invocation.ExitCode $document $invocation.ElapsedMilliseconds
        throw 'unexpected runtime failure envelope'
    }
    $script:rejections.Add([ordered]@{
        operation = $operation
        code = [string]$document.code
    }) | Out-Null
    Complete-AcceptanceStage $stage
}

function Assert-NoListener([int]$Port) {
    $probe = [System.Net.Sockets.TcpClient]::new()
    try {
        $connect = $probe.ConnectAsync([System.Net.IPAddress]::Loopback, $Port)
        try {
            $completed = $connect.Wait(3000)
        } catch [System.AggregateException] {
            $failures = $_.Exception.Flatten().InnerExceptions
            if (
                $failures.Count -eq 1 -and
                $failures[0] -is [System.Net.Sockets.SocketException] -and
                $failures[0].SocketErrorCode -eq [System.Net.Sockets.SocketError]::ConnectionRefused
            ) {
                return
            }
            throw 'listener absence probe failed unexpectedly'
        }
        if (-not $completed) { throw 'listener absence probe exceeded its deadline' }
        throw 'listener remains after authenticated stop'
    } finally {
        $probe.Dispose()
    }
}

function Assert-SameField([object]$Before, [object]$After, [string]$Field, [string]$Label) {
    if ($Before.$Field -cne $After.$Field) { throw "$Label changed $Field" }
}

function Assert-BaselineLifecycleAuthority([object]$Result, [switch]$RequireNoProcesses) {
    if ($null -eq $script:workspaceFilesystemIdentitySha256 -or $null -eq $script:sessionDirectory) {
        throw 'lifecycle authority baseline is absent'
    }
    if (
        [string]$Result.sessionDirectory -cne $script:sessionDirectory -or
        [string]$Result.workspace.filesystemIdentitySha256 -cne $script:workspaceFilesystemIdentitySha256 -or
        [string]$Result.bundle.manifestSha256 -cne $script:manifestSha256 -or
        [string]$Result.bundle.productCommit -cne $script:productCommit
    ) {
        throw 'lifecycle result differs from the retained workspace, session, or bundle authority'
    }
    if ($script:objectWorkspaceDescriptorSha256) {
        if (
            [string]$Result.workspace.objectWorkspaceState -cne 'ACTIVE' -or
            [string]$Result.workspace.objectWorkspace.descriptorHash -cne $script:objectWorkspaceDescriptorSha256
        ) {
            throw 'lifecycle result differs from the governed Object Workspace authority'
        }
    }
    if (
        $RequireNoProcesses -and
        (
            $null -ne $Result.supervisorPid -or
            $null -ne $Result.workbenchPid -or
            $null -ne $Result.applicationIdentitySha256
        )
    ) {
        throw 'non-owned lifecycle classification unexpectedly exposes process identity'
    }
}

function Invoke-BoundedCleanupStop {
    if ($null -eq $script:workspaceFilesystemIdentitySha256 -or $null -eq $script:sessionDirectory) {
        $cleanupStatus = Invoke-Runtime `
            -Arguments @('status', '--workspace', $workspace, '--port', "$port") `
            -StagePrefix 'cleanup' `
            -ArtifactRoot $script:cleanupArtifact `
            -TimeoutMilliseconds 60000
        if ($cleanupStatus.result.classification -cnotin @('OWNED', 'STARTING', 'STOPPING', 'STALE', 'STOPPED')) {
            throw 'cleanup authority probe did not classify one exact stoppable harness state'
        }
        if (
            $cleanupStatus.result.classification -ceq 'STOPPED' -and
            (
                $null -ne $cleanupStatus.result.supervisorPid -or
                $null -ne $cleanupStatus.result.workbenchPid -or
                $null -ne $cleanupStatus.result.applicationIdentitySha256
            )
        ) {
            throw 'stopped cleanup authority probe unexpectedly exposes process identity'
        }
        $candidateSessionDirectory = [string]$cleanupStatus.result.sessionDirectory
        Assert-PrivateSessionDirectory $candidateSessionDirectory
        $script:sessionDirectory = $candidateSessionDirectory
        $script:workspaceFilesystemIdentitySha256 = [string]$cleanupStatus.result.workspace.filesystemIdentitySha256
        Assert-BaselineLifecycleAuthority `
            $cleanupStatus.result `
            -RequireNoProcesses:($cleanupStatus.result.classification -ceq 'STOPPED')
        try {
            Retain-ReportedOwnedProcesses $cleanupStatus.result 'cleanup status' | Out-Null
        } catch {
            Write-Warning 'cleanup status process handle retention was not proven; bounded stop will still run'
        }
    }
    $stage = 'cleanup'
    Set-AcceptanceStage $stage
    $script:commandSequence++
    try {
        $invocation = Invoke-NativeRuntimeProcess `
            (Join-Path $script:cleanupArtifact 'migration-stream-local.exe') `
            @('stop', '--workspace', $workspace, '--port', "$port") `
            60000
    } catch {
        if ($_.Exception.Message -ceq 'RUNTIME_PROCESS_TREE_CLEANUP_UNPROVEN') {
            $script:processCleanupUnproven = $true
        }
        throw
    }
    if ($null -ne $invocation.FailureCode -or $invocation.ExitCode -ne 0) {
        throw 'bounded cleanup command failed'
    }
    try {
        $document = $invocation.StandardOutput | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw 'bounded cleanup command did not emit JSON'
    }
    if (
        $document.schemaVersion -isnot [string] -or
        $document.schemaVersion -cne 'migration-stream/assisted-local-runtime/cli/v1' -or
        $document.ok -isnot [bool] -or
        $document.ok -ne $true -or
        $document.result.schemaVersion -cne 'migration-stream/assisted-local-runtime/status/v2' -or
        $document.result.classification -cne 'STOPPED' -or
        [string]$document.result.listenAddress -cne "127.0.0.1:$port" -or
        [string]$document.result.url -cne "http://127.0.0.1:$port"
    ) {
        throw 'bounded cleanup command emitted an invalid envelope'
    }
    Assert-BaselineLifecycleAuthority $document.result -RequireNoProcesses
    Assert-NoListener $port
    Complete-AcceptanceStage $stage
}

try {
    Write-AcceptanceEvidence
    $pwshExecutable = [Environment]::ProcessPath
    if (
        [string]::IsNullOrWhiteSpace($pwshExecutable) -or
        -not [System.IO.Path]::IsPathFullyQualified($pwshExecutable) -or
        -not (Test-Path -LiteralPath $pwshExecutable -PathType Leaf) -or
        [System.IO.Path]::GetFileName($pwshExecutable) -ine 'pwsh.exe'
    ) {
        throw 'current PowerShell executable is not an exact pwsh.exe path'
    }
    foreach ($command in @('python', 'npm', 'node', 'cargo', 'rustc')) {
        if (-not (Get-Command $command -CommandType Application -ErrorAction SilentlyContinue)) {
            throw "missing native acceptance prerequisite: $command"
        }
    }
    $rustVerbose = @(& rustc -vV)
    if ($LASTEXITCODE -ne 0) { throw 'rustc host inspection failed' }
    $rustHostLines = @($rustVerbose | Where-Object { $_ -clike 'host: *' })
    $nodeArchitecture = (& node -p 'process.arch').Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Node architecture inspection failed' }
    $pythonFacts = @(& python -c "import platform,struct; print(platform.machine()); print(struct.calcsize('P') * 8); print(platform.python_version())")
    if ($LASTEXITCODE -ne 0 -or $pythonFacts.Count -ne 3) { throw 'Python architecture inspection failed' }
    if (
        [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() -cne $targetContract.Runtime -or
        [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString() -cne $targetContract.Runtime -or
        $env:PROCESSOR_ARCHITECTURE -cne $targetContract.Processor -or
        $rustHostLines.Count -ne 1 -or
        $rustHostLines[0] -cne "host: $Target" -or
        $nodeArchitecture -cne $targetContract.Node -or
        $pythonFacts[0].Trim() -cne $targetContract.Processor -or
        $pythonFacts[1].Trim() -cne '64'
    ) {
        throw 'native acceptance host, process, Rust, or Node architecture differs from the exact target'
    }
    $hostFacts = [ordered]@{
        osArchitecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        processArchitecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
        processorArchitecture = $env:PROCESSOR_ARCHITECTURE
        rustHost = $Target
        nodeArchitecture = $nodeArchitecture
        pythonArchitecture = $pythonFacts[0].Trim()
        pwshVersion = $PSVersionTable.PSVersion.ToString()
        rustcVersion = ((& rustc -V) -join '').Trim()
        cargoVersion = ((& cargo -V) -join '').Trim()
        nodeVersion = ((& node --version) -join '').Trim()
        npmVersion = ((& npm --version) -join '').Trim()
        pythonVersion = $pythonFacts[2].Trim()
        nativeHostMatched = $true
    }
} catch {
    try {
        if ($null -eq $runtimeFailure) {
            Set-HarnessFailureEvidence `
                'script-initialization' `
                'RUNTIME_PREFLIGHT_FAILED' `
                'native acceptance preflight failed' `
                $acceptanceTimer.ElapsedMilliseconds
        }
    } finally {
        if ($null -ne $summaryStream) {
            $summaryStream.Dispose()
            $summaryStream = $null
        }
        $acceptanceTimer.Stop()
    }
    throw 'Windows native acceptance preflight failed'
}
$candidateEnvironmentBefore = [Environment]::GetEnvironmentVariable(
    'MIGRATION_STREAM_ENABLE_WINDOWS_NATIVE_ACCEPTANCE_CANDIDATE',
    'Process'
)

$workCreated = $false
$locationPushed = $false
try {
    $tempRootCreationItem = Get-Item -LiteralPath $tempRoot -Force
    if (
        -not $tempRootCreationItem.PSIsContainer -or
        ($tempRootCreationItem.Attributes -band [IO.FileAttributes]::ReparsePoint)
    ) {
        throw 'system temporary root changed before harness work-root creation'
    }
    $tempRootAuthorityFact = Get-WindowsObjectAuthorityFact $tempRootCreationItem
    New-Item -ItemType Directory -Path $work | Out-Null
    $workCreated = $true
    $workCreationItem = Get-Item -LiteralPath $work -Force
    if (
        -not $workCreationItem.PSIsContainer -or
        ($workCreationItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $workCreationItem.Name -cne $workName -or
        -not [string]::Equals($workCreationItem.Parent.FullName, $tempRoot, [StringComparison]::OrdinalIgnoreCase)
    ) {
        throw 'harness work root differs immediately after creation'
    }
    $workRootAuthorityFact = Get-WindowsObjectAuthorityFact $workCreationItem
    $workRootDisposition = 'ACTIVE'
    New-Item -ItemType Directory -Path $workspace | Out-Null
    Write-AcceptanceEvidence
    Push-Location $root
    $locationPushed = $true
    $env:MIGRATION_STREAM_ENABLE_WINDOWS_NATIVE_ACCEPTANCE_CANDIDATE = '1'
    Set-AcceptanceStage 'bundle-build'
    $buildScript = (Resolve-Path (Join-Path $PSScriptRoot 'build-assisted-runtime-bundle.ps1')).Path
    $buildArguments = @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-File', $buildScript,
        '-Output', $artifact, '-Target', $Target,
        '-AssemblerResultFile', $assemblerResultFile,
        '-RetainedBuildRoot', $retainedBuildRoot
    )
    if ($AllowUncommittedSnapshot) { $buildArguments += '-AllowUncommittedSnapshot' }
    Invoke-BoundedInheritedProcess `
        'bundle-build' `
        $pwshExecutable `
        $buildArguments `
        (30 * 60 * 1000)
    $retainedBuildRootItem = Get-Item -LiteralPath $retainedBuildRoot -Force
    if (
        -not $retainedBuildRootItem.PSIsContainer -or
        ($retainedBuildRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)
    ) {
        throw 'builder did not retain one real private evidence root'
    }
    $builderPrivateRootRetained = $true
    $assemblyResult = Read-AssemblerResultEvidence $assemblerResultFile $artifact
    $releaseExecutableCount = 4
    $allReleaseExecutablesNative = $true
    Complete-AcceptanceStage 'bundle-build'
    $acceptanceDeadline = [DateTimeOffset]::UtcNow.AddMinutes(15)
    Write-AcceptanceEvidence

    Set-AcceptanceStage 'assembled-inspection'
    $assembledArtifact = $artifact
    $assembledManifest = Read-BundleManifestEvidence $assembledArtifact
    Assert-LowerHex $assembledManifest.ManifestSha256 64 'manifest SHA-256'
    Assert-LowerHex $assembledManifest.ProductCommit 40 'product commit'
    Assert-LowerHex $assembledManifest.SourceSnapshotSha256 64 'source snapshot SHA-256'
    $manifestSha256 = $assembledManifest.ManifestSha256
    $productCommit = $assembledManifest.ProductCommit
    $sourceState = $assembledManifest.SourceState
    $sourceSnapshotSha256 = $assembledManifest.SourceSnapshotSha256
    $bundleMemberCount = $assembledManifest.MemberCount
    $totalMemberBytes = $assembledManifest.TotalMemberBytes
    if (
        $manifestSha256 -cne [string]$assemblyResult.manifestSha256 -or
        [uint64]$bundleMemberCount -ne [uint64]$assemblyResult.memberCount -or
        [uint64]$totalMemberBytes -ne [uint64]$assemblyResult.totalMemberBytes
    ) {
        throw 'assembled manifest identity differs from the assembler success result'
    }
    $shippedExecutableCount = Assert-BundleExecutableInventory $assembledArtifact
    $allShippedExecutablesNative = $true
    $launcherControllerByteIdentical = $true
    Complete-AcceptanceStage 'assembled-inspection'
    Invoke-Runtime -Arguments @('verify') -StagePrefix 'assembled' | Out-Null
    Invoke-Runtime -Controller -Arguments @('verify') -StagePrefix 'assembled' | Out-Null
    $assembledManifestReplayOk = $true

    Set-AcceptanceStage 'archive-replay'
    $archive = "$assembledArtifact.zip"
    $archiveSha256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-LowerHex $archiveSha256 64 'archive SHA-256'
    if ($archiveSha256 -cne [string]$assemblyResult.archiveSha256) {
        throw 'published archive identity differs from the assembler semantic replay result'
    }
    $expectedSidecar = [Text.UTF8Encoding]::new($false).GetBytes(
        "$archiveSha256  $($targetContract.Artifact).zip`n"
    )
    $observedSidecar = [IO.File]::ReadAllBytes("$archive.sha256")
    if ([Convert]::ToHexString($observedSidecar) -cne [Convert]::ToHexString($expectedSidecar)) {
        throw 'archive checksum sidecar differs from the exact archive identity'
    }
    $checksumSidecarMatched = $true
    $assemblerArchiveReplayOk = $true
    Complete-AcceptanceStage 'archive-replay'

    Set-AcceptanceStage 'archive-unpack'
    $unpacked = Join-Path $work 'unpacked'
    New-Item -ItemType Directory -Path $unpacked | Out-Null
    $archiveEntryCount = Expand-VerifiedBundleArchive $archive $unpacked $assembledManifest
    $unpackedRoots = @(Get-ChildItem -LiteralPath $unpacked -Force)
    if (
        $unpackedRoots.Count -ne 1 -or
        -not $unpackedRoots[0].PSIsContainer -or
        ($unpackedRoots[0].Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        $unpackedRoots[0].Name -cne $targetContract.Artifact
    ) {
        throw 'archive did not unpack to one exact canonical real root'
    }
    $artifact = $unpackedRoots[0].FullName
    Complete-AcceptanceStage 'archive-unpack'

    Set-AcceptanceStage 'unpacked-inspection'
    $unpackedManifest = Read-BundleManifestEvidence $artifact
    foreach ($field in @('ManifestSha256', 'ProductCommit', 'SourceState', 'SourceSnapshotSha256', 'MemberCount', 'TotalMemberBytes', 'RawFileLength', 'RawFileSha256')) {
        Assert-SameField $assembledManifest $unpackedManifest $field 'unpacked manifest replay'
    }
    $unpackedExecutableCount = Assert-BundleExecutableInventory $artifact
    if ($unpackedExecutableCount -ne $shippedExecutableCount) {
        throw 'unpacked executable count differs from the assembled bundle'
    }
    Complete-AcceptanceStage 'unpacked-inspection'
    Invoke-Runtime -Arguments @('verify') -StagePrefix 'unpacked' | Out-Null
    Invoke-Runtime -Controller -Arguments @('verify') -StagePrefix 'unpacked' | Out-Null
    $unpackedManifestReplayOk = $true
    $rootVerifyOk = $true
    $controllerVerifyOk = $true
    $cleanupArtifact = $artifact
    $bundleEntrypointsVerified = $true

    $port = Select-HarnessOwnedPort $Port
    $lifecycleStartAttempted = $true
    $firstStart = Invoke-Runtime -Arguments @('start', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'OWNED'
    $firstStartProcesses = Retain-ReportedOwnedProcesses $firstStart.result 'initial start'
    $sessionDirectory = [string]$firstStart.result.sessionDirectory
    Assert-PrivateSessionDirectory $sessionDirectory
    $workspaceFilesystemIdentitySha256 = [string]$firstStart.result.workspace.filesystemIdentitySha256
    $beforeInitialize = Invoke-Runtime -Arguments @('status', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'OWNED'
    $controllerStatus = Invoke-Runtime -Controller -Arguments @('status', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'OWNED'
    foreach ($field in @('supervisorPid', 'workbenchPid', 'applicationIdentitySha256')) { Assert-SameField $firstStart.result $beforeInitialize.result $field 'initial status' }
    foreach ($field in @('supervisorPid', 'workbenchPid', 'applicationIdentitySha256')) { Assert-SameField $beforeInitialize.result $controllerStatus.result $field 'direct controller status' }
    $baseUri = "http://127.0.0.1:$port"
    Set-AcceptanceStage 'object-workspace-get'
    $workspaceState = Invoke-RestMethod -Method Get -Uri "$baseUri/api/v1/object-workspace" -TimeoutSec (Get-HttpTimeoutSeconds) -ErrorAction Stop
    $stateHash = $workspaceState.stateHash
    if (
        $workspaceState.schemaVersion -cne 'migration-stream/object-workspace/status/v1' -or
        $workspaceState.state -cne 'INITIALIZE_REQUIRED' -or
        $null -ne $workspaceState.workspace
    ) {
        throw 'fresh Object Workspace status differs from the exact initialization contract'
    }
    Assert-LowerHex $stateHash 64 'fresh Object Workspace state hash'
    Complete-AcceptanceStage 'object-workspace-get'
    $idempotencyKey = "windows-$($targetContract.Architecture)-object-workspace-initialize-v1"
    $creatorAttribution = "windows-$($targetContract.Architecture)-local"
    $initialize = @{ schemaVersion = 'migration-stream/object-workspace/write/initialize/v1'; expectedStateHash = $stateHash; displayName = 'Windows native acceptance'; migrationObjectNameOrScope = 'synthetic-fresh-workspace'; creatorAttribution = $creatorAttribution; idempotencyKey = $idempotencyKey } | ConvertTo-Json -Compress
    $headers = @{ 'Accept' = 'application/json'; 'Origin' = $baseUri; 'Idempotency-Key' = $idempotencyKey; 'x-migration-stream-intent' = 'object-workspace-initialize-v1' }
    Set-AcceptanceStage 'object-workspace-initialize'
    $initialized = Invoke-RestMethod -Method Post -Uri "$baseUri/api/v1/object-workspace/initialize" -Headers $headers -ContentType 'application/json' -Body $initialize -TimeoutSec (Get-HttpTimeoutSeconds) -ErrorAction Stop
    if ($initialized.workspaceId -cnotmatch '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' -or $initialized.schemaVersion -cne 'migration-stream/object-workspace/v1' -or $initialized.generation -ne 1 -or $initialized.displayName -cne 'Windows native acceptance' -or $initialized.migrationObjectNameOrScope -cne 'synthetic-fresh-workspace' -or $initialized.creatorAttribution -cne $creatorAttribution -or $initialized.establishedBy -cne 'INITIALIZED') { throw 'object workspace initialize record contract differs' }
    Assert-LowerHex $initialized.descriptorHash 64 'initialized Object Workspace descriptor hash'
    Complete-AcceptanceStage 'object-workspace-initialize'
    Set-AcceptanceStage 'object-workspace-active'
    $activeWorkspace = Invoke-RestMethod -Method Get -Uri "$baseUri/api/v1/object-workspace" -Headers @{ 'Accept' = 'application/json'; 'Origin' = $baseUri } -TimeoutSec (Get-HttpTimeoutSeconds) -ErrorAction Stop
    if ($activeWorkspace.schemaVersion -cne 'migration-stream/object-workspace/status/v1' -or $activeWorkspace.state -cne 'ACTIVE' -or $null -ne $activeWorkspace.requiredAction -or $null -ne $activeWorkspace.unavailableReason -or $null -ne $activeWorkspace.pendingCommand -or $activeWorkspace.workspace.workspaceId -cne $initialized.workspaceId -or $activeWorkspace.workspace.schemaVersion -cne 'migration-stream/object-workspace/v1' -or $activeWorkspace.workspace.generation -ne 1 -or $activeWorkspace.workspace.displayName -cne 'Windows native acceptance' -or $activeWorkspace.workspace.migrationObjectNameOrScope -cne 'synthetic-fresh-workspace' -or $activeWorkspace.workspace.creatorAttribution -cne $creatorAttribution -or $activeWorkspace.workspace.descriptorHash -cne $initialized.descriptorHash) { throw 'active object workspace contract differs' }
    Assert-LowerHex $activeWorkspace.stateHash 64 'active Object Workspace state hash'
    $objectWorkspaceDescriptorSha256 = [string]$initialized.descriptorHash
    $workspaceInitialized = $true
    Complete-AcceptanceStage 'object-workspace-active'

    $afterInitialize = Invoke-Runtime -Arguments @('status', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'OWNED'
    if ($afterInitialize.result.workspace.objectWorkspaceState -ne 'ACTIVE') { throw 'lifecycle status did not observe the active object workspace' }
    if ($afterInitialize.result.applicationIdentitySha256 -eq $beforeInitialize.result.applicationIdentitySha256) { throw 'governed initialization did not advance live application identity' }
    $liveIdentityChangedAfterInitialize = $true
    $secondStart = Invoke-Runtime -Arguments @('start', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'OWNED'
    foreach ($field in @('supervisorPid', 'workbenchPid', 'applicationIdentitySha256')) { Assert-SameField $afterInitialize.result $secondStart.result $field 'idempotent start' }
    Assert-SameField $afterInitialize.result.workspace $secondStart.result.workspace 'filesystemIdentitySha256' 'idempotent start'
    Assert-SameField $afterInitialize.result.bundle $secondStart.result.bundle 'manifestSha256' 'idempotent start'
    Assert-SameField $afterInitialize.result.bundle $secondStart.result.bundle 'productCommit' 'idempotent start'
    $idempotentIdentityPreserved = $true

    $preRestartSupervisor = $firstStartProcesses.Supervisor
    $preRestartWorkbench = $firstStartProcesses.Workbench
    $restart = Invoke-Runtime -Arguments @('restart', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'OWNED'
    $replacementProcesses = Retain-ReportedOwnedProcesses $restart.result 'replacement runtime'
    $replacementSupervisor = $replacementProcesses.Supervisor
    $replacementWorkbench = $replacementProcesses.Workbench
    Assert-SameField $afterInitialize.result $restart.result 'applicationIdentitySha256' 'restart'
    Assert-SameField $afterInitialize.result.workspace $restart.result.workspace 'filesystemIdentitySha256' 'restart'
    Assert-SameField $afterInitialize.result.workspace.objectWorkspace $restart.result.workspace.objectWorkspace 'descriptorHash' 'restart'
    Assert-SameField $afterInitialize.result.bundle $restart.result.bundle 'manifestSha256' 'restart'
    Assert-SameField $afterInitialize.result.bundle $restart.result.bundle 'productCommit' 'restart'
    if (
        $restart.result.supervisorPid -eq $afterInitialize.result.supervisorPid -or
        $restart.result.workbenchPid -eq $afterInitialize.result.workbenchPid
    ) {
        throw 'restart did not replace both owned processes'
    }
    Assert-ProcessExited $preRestartSupervisor 'pre-restart supervisor'
    Assert-ProcessExited $preRestartWorkbench 'pre-restart workbench'
    $restartIdentityPreserved = $true
    $restartProcessesReplaced = $true

    $beforeStop = Invoke-Runtime -Arguments @('status', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'OWNED'
    foreach ($field in @('supervisorPid', 'workbenchPid', 'applicationIdentitySha256')) { Assert-SameField $restart.result $beforeStop.result $field 'pre-stop continuity' }
    Invoke-Runtime -Arguments @('stop', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'STOPPED' | Out-Null
    $authenticatedLifecycleStopOk = $true
    Assert-NoListener $port
    Assert-ProcessExited $replacementSupervisor 'replacement supervisor'
    Assert-ProcessExited $replacementWorkbench 'replacement workbench'
    $ownedProcessesExited = $true
    Assert-StoppedSessionState $sessionDirectory
    $protectedRecordsAbsent = $true
    $mainListenerStopped = $true
    Invoke-Runtime -Arguments @('status', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'STOPPED' | Out-Null
    Assert-StoppedSessionState $sessionDirectory
    $workspaceBeforeIntegrity = Get-BoundedTreeDigest $workspace
    $sessionBeforeIntegrity = Get-BoundedTreeDigest $sessionDirectory
    Invoke-Runtime -Arguments @('integrity', '--workspace', $workspace, '--port', "$port") | Out-Null
    Invoke-Runtime -Arguments @('status', '--workspace', $workspace, '--port', "$port") -ExpectedClassification 'STOPPED' | Out-Null
    Assert-NoListener $port
    Assert-StoppedSessionState $sessionDirectory
    if (
        (Get-BoundedTreeDigest $workspace) -cne $workspaceBeforeIntegrity -or
        (Get-BoundedTreeDigest $sessionDirectory) -cne $sessionBeforeIntegrity
    ) {
        throw 'stopped integrity replay mutated harness-owned state'
    }
    $integrityOk = $true
    $persistentLocksReplayed = $true

    do {
        Set-AcceptanceStage 'foreign-listener'
        if ($foreignListener) { $foreignListener.Stop() }
        $foreignListener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
        $foreignListener.Start()
        $foreignListenerCreated = $true
        $foreignPort = ([System.Net.IPEndPoint]$foreignListener.LocalEndpoint).Port
    } while ($foreignPort -eq $port)
    Complete-AcceptanceStage 'foreign-listener'
    Assert-ListenerConnectable $foreignPort
    $workspaceBeforeForeign = Get-BoundedTreeDigest $workspace
    $sessionBeforeForeign = Get-BoundedTreeDigest $sessionDirectory
    $foreignStatus = Invoke-Runtime -Arguments @('status', '--workspace', $workspace, '--port', "$foreignPort") -ExpectedClassification 'FOREIGN_LISTENER'
    if ([string]$foreignStatus.result.sessionDirectory -cne $sessionDirectory) {
        throw 'foreign-listener classification selected a different private session'
    }
    Assert-ListenerConnectable $foreignPort
    if (
        (Get-BoundedTreeDigest $workspace) -cne $workspaceBeforeForeign -or
        (Get-BoundedTreeDigest $sessionDirectory) -cne $sessionBeforeForeign
    ) {
        throw 'initial foreign-listener status mutated harness-owned state'
    }
    Assert-StoppedSessionState $sessionDirectory
    foreach ($command in @('start', 'restart', 'stop')) {
        Invoke-RuntimeFailure -Arguments @($command, '--workspace', $workspace, '--port', "$foreignPort")
        Assert-ListenerConnectable $foreignPort
        if (
            (Get-BoundedTreeDigest $workspace) -cne $workspaceBeforeForeign -or
            (Get-BoundedTreeDigest $sessionDirectory) -cne $sessionBeforeForeign
        ) {
            throw 'foreign-listener refusal mutated harness-owned state before status replay'
        }
        $replayedForeignStatus = Invoke-Runtime `
            -Arguments @('status', '--workspace', $workspace, '--port', "$foreignPort") `
            -ExpectedClassification 'FOREIGN_LISTENER'
        Assert-ListenerConnectable $foreignPort
        if ([string]$replayedForeignStatus.result.sessionDirectory -cne $sessionDirectory) {
            throw 'foreign-listener replay selected a different private session'
        }
        if (
            (Get-BoundedTreeDigest $workspace) -cne $workspaceBeforeForeign -or
            (Get-BoundedTreeDigest $sessionDirectory) -cne $sessionBeforeForeign
        ) {
            throw 'foreign-listener refusal mutated harness-owned state'
        }
        Assert-StoppedSessionState $sessionDirectory
    }
    Invoke-Runtime -Arguments @('status', '--workspace', $workspace, '--port', "$foreignPort") -ExpectedClassification 'FOREIGN_LISTENER' | Out-Null
    Assert-ListenerConnectable $foreignPort
    if (
        (Get-BoundedTreeDigest $workspace) -cne $workspaceBeforeForeign -or
        (Get-BoundedTreeDigest $sessionDirectory) -cne $sessionBeforeForeign
    ) {
        throw 'final foreign-listener status mutated harness-owned state'
    }
    Assert-StoppedSessionState $sessionDirectory
    $foreignListenerPreserved = $true
    $foreignStateUnchanged = $true
    $lifecycleAuthorityContinuous = $true
    if ($Target -eq 'x86_64-pc-windows-msvc') {
        Set-AcceptanceStage 'project-runtime-bridge'
        $bridgeBundleBefore = $env:MIGRATION_STREAM_TEST_PROJECT_RUNTIME_BUNDLE
        $bridgeEvidenceBefore = $env:MIGRATION_STREAM_TEST_PROJECT_RUNTIME_EVIDENCE_DIR
        try {
            $env:MIGRATION_STREAM_TEST_PROJECT_RUNTIME_BUNDLE = $artifact
            $env:MIGRATION_STREAM_TEST_PROJECT_RUNTIME_EVIDENCE_DIR = Join-Path $evidenceRoot (
                [IO.Path]::GetFileNameWithoutExtension($EvidenceFileName) + '-project-runtime-bridge'
            )
            cargo test --locked --test project_runtime_bridge -- --ignored --exact bound_project_real_start_reuse_stop_reopen_and_foreign_isolation
            if ($LASTEXITCODE -ne 0) { throw 'packaged project runtime bridge proof failed' }
            $projectRuntimeBridgeOk = $true
            Complete-AcceptanceStage 'project-runtime-bridge'
        } finally {
            $env:MIGRATION_STREAM_TEST_PROJECT_RUNTIME_BUNDLE = $bridgeBundleBefore
            $env:MIGRATION_STREAM_TEST_PROJECT_RUNTIME_EVIDENCE_DIR = $bridgeEvidenceBefore
        }
    }
    $lifecycleSucceeded = $true
} catch {
    if ($null -eq $runtimeFailure) {
        Set-HarnessFailureEvidence $currentStage 'RUNTIME_STAGE_FAILED' 'native acceptance stage reported failure' $acceptanceTimer.ElapsedMilliseconds
    }
    throw "Windows native acceptance failed during stage: $currentStage"
} finally {
    $cleanupSafetyMode = $true
    if (
        $lifecycleStartAttempted -and
        $bundleEntrypointsVerified -and
        $cleanupArtifact -and
        (Test-Path (Join-Path $cleanupArtifact 'migration-stream-local.exe'))
    ) {
        $cleanupAttempted = $true
        $boundedCleanupStopOk = $false
        $ownedProcessesExited = $false
        $protectedRecordsAbsent = $false
        $persistentLocksReplayed = $false
        $mainListenerAbsent = $false
        try {
            Invoke-BoundedCleanupStop
            $boundedCleanupStopOk = $true
            $mainListenerAbsent = $true
            if ($ownedProcessHandleProofIncomplete -or $retainedOwnedProcesses.Count -eq 0) {
                throw 'the complete exact owned process handle set was not retained for cleanup proof'
            }
            foreach ($ownedProcess in $retainedOwnedProcesses) { Assert-ProcessExited $ownedProcess 'retained owned process' }
            $ownedProcessesExited = $true
            if ($sessionDirectory) {
                Assert-StoppedSessionState $sessionDirectory
                $protectedRecordsAbsent = $true
                Invoke-Runtime `
                    -Arguments @('status', '--workspace', $workspace, '--port', "$port") `
                -ExpectedClassification 'STOPPED' `
                -StagePrefix 'cleanup' `
                -ArtifactRoot $cleanupArtifact `
                -TimeoutMilliseconds 60000 | Out-Null
                Assert-StoppedSessionState $sessionDirectory
                $persistentLocksReplayed = $true
            }
        } catch {
            Set-CleanupFailureEvidence 'RUNTIME_CLEANUP_FAILED' 'bounded runtime cleanup was not proven'
            Write-Warning 'bounded acceptance cleanup stop failed'
        }
    }
    if ($foreignListener) {
        try {
            $foreignListener.Stop()
            Assert-NoListener $foreignPort
            $foreignListenerReleased = $true
        } catch {
            Set-CleanupFailureEvidence 'RUNTIME_FOREIGN_LISTENER_CLEANUP_FAILED' 'harness-owned listener cleanup was not proven'
        }
    } elseif (-not $foreignListenerCreated) {
        $foreignListenerReleased = $true
    }
    foreach ($ownedProcess in $retainedOwnedProcesses) { $ownedProcess.Dispose() }
    if ($null -eq $candidateEnvironmentBefore) {
        Remove-Item Env:MIGRATION_STREAM_ENABLE_WINDOWS_NATIVE_ACCEPTANCE_CANDIDATE -ErrorAction SilentlyContinue
    } else {
        $env:MIGRATION_STREAM_ENABLE_WINDOWS_NATIVE_ACCEPTANCE_CANDIDATE = $candidateEnvironmentBefore
    }
    if ($locationPushed) { Pop-Location }

    $runtimeResourcesOk = $cleanupAttempted -and $boundedCleanupStopOk -and $mainListenerAbsent -and
        $ownedProcessesExited -and $protectedRecordsAbsent -and $persistentLocksReplayed -and
        -not $processCleanupUnproven -and $foreignListenerReleased -and
        $null -eq $cleanupFailure
    $cleanupOk = $runtimeResourcesOk
    if ($workCreated) {
        try {
            $tempRootBeforeRetention = Get-Item -LiteralPath $tempRoot -Force
            $workItem = Get-Item -LiteralPath $work -Force
            if (
                $null -eq $tempRootAuthorityFact -or
                $null -eq $workRootAuthorityFact -or
                -not $tempRootBeforeRetention.PSIsContainer -or
                ($tempRootBeforeRetention.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                (Get-WindowsObjectAuthorityFact $tempRootBeforeRetention) -cne $tempRootAuthorityFact -or
                -not $workItem.PSIsContainer -or
                ($workItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                $workItem.Name -cne $workName -or
                -not [string]::Equals($workItem.Parent.FullName, $tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
                (Get-WindowsObjectAuthorityFact $workItem) -cne $workRootAuthorityFact
            ) {
                throw 'harness work root identity differs from its exact retained boundary'
            }
            $workDigestMaximumEntries = if ($archiveEntryCount -gt 0) {
                (3 * $archiveEntryCount) + 1024
            } else {
                8192
            }
            [uint64]$workDigestMaximumBytes = if (
                $bundleMemberCount -gt 0 -and
                $assembledManifest -and
                $archive -and
                (Test-Path -LiteralPath $archive)
            ) {
                (2 * [uint64]$totalMemberBytes) +
                    (2 * [uint64]$assembledManifest.RawFileLength) +
                    [uint64](Get-Item -LiteralPath $archive -Force).Length +
                    1GB
            } else {
                2GB
            }
            $workRootIdentitySha256 = Get-BoundedTreeDigest `
                -TreeRoot $work `
                -MaximumEntries $workDigestMaximumEntries `
                -MaximumMemberBytes 512MB `
                -MaximumTotalBytes $workDigestMaximumBytes
            $tempRootAfterRetention = Get-Item -LiteralPath $tempRoot -Force
            $workItemAfterRetention = Get-Item -LiteralPath $work -Force
            if (
                -not $tempRootAfterRetention.PSIsContainer -or
                ($tempRootAfterRetention.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                (Get-WindowsObjectAuthorityFact $tempRootAfterRetention) -cne $tempRootAuthorityFact -or
                -not $workItemAfterRetention.PSIsContainer -or
                ($workItemAfterRetention.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                $workItemAfterRetention.Name -cne $workName -or
                -not [string]::Equals($workItemAfterRetention.Parent.FullName, $tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
                (Get-WindowsObjectAuthorityFact $workItemAfterRetention) -cne $workRootAuthorityFact
            ) {
                throw 'harness work root identity changed during retained-tree replay'
            }
            $workRootDisposition = if (
                -not $processCleanupUnproven -and
                (-not $lifecycleStartAttempted -or $runtimeResourcesOk)
            ) {
                'RETAINED_FOR_EXACT_RECOVERY'
            } else {
                'RETAINED_AFTER_UNPROVEN_CLEANUP'
            }
        } catch {
            $workRootDisposition = 'RETAINED_AFTER_UNPROVEN_CLEANUP'
            Set-CleanupFailureEvidence 'RUNTIME_WORK_ROOT_RETENTION_UNPROVEN' 'harness work-root retention identity was not proven'
            $cleanupOk = $false
        }
    }
    if ($lifecycleSucceeded -and $cleanupOk -and $workRootDisposition -ceq 'RETAINED_FOR_EXACT_RECOVERY') {
        $succeeded = $true
        $currentStage = 'complete'
        $completedStages.Add('complete') | Out-Null
    }
    try { Write-AcceptanceEvidence } finally {
        if ($null -ne $summaryStream) {
            $summaryStream.Dispose()
            $summaryStream = $null
        }
        $acceptanceTimer.Stop()
    }
}
if ($lifecycleSucceeded -and -not $succeeded) {
    throw 'Windows native acceptance cleanup did not reach a proven terminal state'
}
