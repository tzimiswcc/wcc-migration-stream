[CmdletBinding()]
param(
    [ValidateSet('Pack', 'Extract')][string]$Operation,
    [string]$Layout,
    [string]$BuildReceipt,
    [string]$Archive,
    [string]$ExpectedArchiveSha256,
    [string]$Destination
)

# Purpose-only Windows byte-container. No attribute/ACL/SID restoration.
$ErrorActionPreference = 'Stop'
$script:PreviewMaximumArchive = 1GB
$script:PreviewMaximumExpanded = 642MB
$script:PreviewMaximumMember = 128MB
$script:PreviewMaximumReceipt = 1MB
$script:PreviewMaximumEntries = 4096
$script:PreviewReceiptName = 'preview-receipt.json'

function Assert-Preview([bool]$Condition) {
    if (-not $Condition) { throw 'engineering candidate refused' }
}
function Get-PreviewFullPath([string]$Path) {
    Assert-Preview ([IO.Path]::IsPathFullyQualified($Path) -and $Path -cmatch '^[A-Za-z]:\\')
    Assert-Preview (@($Path.Split('\') | Where-Object { $_ -in @('.', '..') }).Count -eq 0)
    return [IO.Path]::GetFullPath($Path).TrimEnd('\')
}
function Assert-PreviewNative([string]$Path, [bool]$Directory) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    Assert-Preview ($item.PSIsContainer -eq $Directory)
    if (-not $Directory) { Assert-Preview ($item -is [IO.FileInfo]) }
    while ($null -ne $item) {
        Assert-Preview (-not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))
        Assert-Preview ([string]::IsNullOrEmpty([string]$item.LinkType))
        $item = if ($item -is [IO.FileInfo]) { $item.Directory } else { $item.Parent }
    }
}
function Assert-PreviewName([string]$Name) {
    Assert-Preview ($Name.Length -ge 1 -and [Text.Encoding]::UTF8.GetByteCount($Name) -le 1024)
    Assert-Preview (-not $Name.Contains('\') -and -not $Name.Contains(':'))
    $trimmed = $Name.TrimEnd('/')
    Assert-Preview ($Name -ceq $trimmed -or $Name -ceq ($trimmed + '/'))
    $parts = $trimmed.Split('/')
    Assert-Preview ($parts.Count -le 66)
    foreach ($part in $parts) {
        Assert-Preview ($part -cmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$' -and -not $part.EndsWith('.'))
        Assert-Preview ($part -notmatch '^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(\..*)?$')
    }
}
function Get-PreviewStreamHash([IO.Stream]$Stream, [long]$Maximum) {
    $digest = [Security.Cryptography.IncrementalHash]::CreateHash([Security.Cryptography.HashAlgorithmName]::SHA256)
    try {
        $buffer = [byte[]]::new(65536)
        [long]$total = 0
        while (($count = $Stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $count
            Assert-Preview ($total -le $Maximum)
            $digest.AppendData($buffer, 0, $count)
        }
        return [BitConverter]::ToString($digest.GetHashAndReset()).Replace('-', '').ToLowerInvariant()
    } finally { $digest.Dispose() }
}
function Get-PreviewHash([string]$Path, [long]$Maximum = 128MB) {
    Assert-PreviewNative $Path $false
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        Assert-Preview ($stream.Length -le $Maximum)
        return Get-PreviewStreamHash $stream $Maximum
    } finally { $stream.Dispose() }
}
function Read-PreviewBytes([IO.Stream]$Stream, [long]$Maximum) {
    $memory = [IO.MemoryStream]::new()
    try {
        $buffer = [byte[]]::new(65536)
        while (($count = $Stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            Assert-Preview ($memory.Length + $count -le $Maximum)
            $memory.Write($buffer, 0, $count)
        }
        return ,$memory.ToArray()
    } finally { $memory.Dispose() }
}
function Assert-PreviewJsonKeys([Text.Json.JsonElement]$Element) {
    if ($Element.ValueKind -eq [Text.Json.JsonValueKind]::Object) {
        $keys = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($property in $Element.EnumerateObject()) {
            Assert-Preview ($keys.Add($property.Name))
            Assert-PreviewJsonKeys $property.Value
        }
    } elseif ($Element.ValueKind -eq [Text.Json.JsonValueKind]::Array) {
        foreach ($value in $Element.EnumerateArray()) { Assert-PreviewJsonKeys $value }
    }
}
function ConvertFrom-PreviewJson([byte[]]$Bytes) {
    Assert-Preview ($Bytes.Length -le $script:PreviewMaximumReceipt)
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($Bytes)
    $options = [Text.Json.JsonDocumentOptions]::new()
    $options.MaxDepth = 16
    $document = [Text.Json.JsonDocument]::Parse($text, $options)
    try { Assert-PreviewJsonKeys $document.RootElement } finally { $document.Dispose() }
    return $text | ConvertFrom-Json -Depth 16
}
function Read-PreviewJsonFile([string]$Path) {
    Assert-PreviewNative $Path $false
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try { return ConvertFrom-PreviewJson (Read-PreviewBytes $stream $script:PreviewMaximumReceipt) }
    finally { $stream.Dispose() }
}
function Get-PreviewRows([string]$Root) {
    Assert-PreviewNative $Root $true
    $rows = [Collections.Generic.SortedDictionary[string,object]]::new([StringComparer]::Ordinal)
    $pending = [Collections.Generic.Stack[object]]::new()
    $pending.Push(@($Root, '', 0))
    [long]$bytes = 0
    while ($pending.Count -gt 0) {
        $node = $pending.Pop()
        Assert-Preview ($rows.Count -lt $script:PreviewMaximumEntries -and $node[2] -le 64)
        Assert-PreviewNative $node[0] $true
        $rows.Add($node[1], [ordered]@{ path = $node[1]; type = 'directory'; bytes = 0; sha256 = $null })
        $iterator = [IO.Directory]::EnumerateFileSystemEntries($node[0]).GetEnumerator()
        try {
            while ($iterator.MoveNext()) {
                Assert-Preview ($rows.Count + $pending.Count -lt $script:PreviewMaximumEntries)
                $path = [string]$iterator.Current
                $name = [IO.Path]::GetFileName($path)
                $relative = if ($node[1]) { $node[1] + '/' + $name } else { $name }
                Assert-PreviewName $relative
                $item = Get-Item -LiteralPath $path -Force
                Assert-PreviewNative $path $item.PSIsContainer
                if ($item.PSIsContainer) { $pending.Push(@($path, $relative, ($node[2] + 1))) }
                else {
                    Assert-Preview ($item.Length -le $script:PreviewMaximumMember)
                    $bytes += $item.Length
                    Assert-Preview ($bytes -le $script:PreviewMaximumExpanded)
                    $rows.Add($relative, [ordered]@{ path = $relative; type = 'file'; bytes = $item.Length; sha256 = (Get-PreviewHash $path) })
                }
            }
        } finally { $iterator.Dispose() }
    }
    # No undeclared third child alongside the exact engineering root contract.
    $top = @($rows.Keys | Where-Object { $_ -ne '' -and -not $_.Contains('/') })
    Assert-Preview ($top.Count -eq 2 -and $top -ccontains 'Runtime' -and $top -ccontains 'migration-stream-document-open.exe')
    Assert-Preview ($rows['Runtime'].type -eq 'directory' -and $rows['migration-stream-document-open.exe'].type -eq 'file')
    return ,@($rows.Values)
}
function ConvertTo-PreviewStableJson([object]$Value) { return $Value | ConvertTo-Json -Depth 12 -Compress }
function Get-PreviewVerificationIdentity([object]$Value) {
    return $Value | Select-Object -Property * -ExcludeProperty bundleRoot
}
function Select-PreviewBuildReceipt([object]$Value) {
    Assert-Preview ($Value.schemaVersion -ceq 'migration-stream/windows-engineering-qualification/v1' -and $Value.status -ceq 'PASSED' -and $Value.stage -ceq 'complete' -and $Value.commandExitCode -eq 0)
    $selected = [ordered]@{ schemaVersion=$Value.schemaVersion; status='PASSED' }
    foreach ($field in @('sourceCommit','sourceTree')) {
        Assert-Preview ($Value.$field -is [string] -and $Value.$field -cmatch '^[0-9a-f]{40}$')
        $selected[$field] = $Value.$field
    }
    Assert-Preview ($Value.target -ceq 'x86_64-pc-windows-msvc')
    $selected.target = $Value.target
    foreach ($field in @('sourceSnapshotSha256','frontendAssetsSha256','documentLauncherSha256','engineeringAssemblerSha256','runtimeManifestSha256','runtimeManifestFileSha256','runtimeLauncherSha256','runtimeControllerSha256','integrityCliSha256','workbenchSha256')) {
        Assert-Preview ($Value.$field -is [string] -and $Value.$field -cmatch '^[0-9a-f]{64}$')
        $selected[$field] = $Value.$field
    }
    Assert-Preview (($Value.runtimeMemberCount -is [int] -or $Value.runtimeMemberCount -is [long]) -and $Value.runtimeMemberCount -ge 1 -and $Value.runtimeMemberCount -le 1024)
    Assert-Preview (($Value.runtimeMemberBytes -is [int] -or $Value.runtimeMemberBytes -is [long]) -and $Value.runtimeMemberBytes -ge 1 -and $Value.runtimeMemberBytes -le 512MB)
    $selected.runtimeMemberCount = $Value.runtimeMemberCount
    $selected.runtimeMemberBytes = $Value.runtimeMemberBytes
    return $selected
}
function Invoke-PreviewRuntimeVerification([string]$Root, [object]$Build) {
    Assert-Preview ($IsWindows -and $Build.schemaVersion -ceq 'migration-stream/windows-engineering-qualification/v1' -and $Build.status -ceq 'PASSED')
    Assert-Preview ($Build.sourceCommit -cmatch '^[0-9a-f]{40}$' -and $Build.sourceTree -cmatch '^[0-9a-f]{40}$' -and $Build.target -ceq 'x86_64-pc-windows-msvc')
    $runtime = Join-Path $Root 'Runtime'
    $controller = Join-Path $runtime 'migration-stream-local.exe'
    foreach ($entry in @(@($controller,'runtimeLauncherSha256'),@((Join-Path $runtime 'libexec/migration-stream-local-runtime.exe'),'runtimeControllerSha256'),@((Join-Path $Root 'migration-stream-document-open.exe'),'documentLauncherSha256'))) {
        Assert-Preview ($Build.($entry[1]) -cmatch '^[0-9a-f]{64}$')
        Assert-Preview ((Get-PreviewHash $entry[0]) -ceq $Build.($entry[1]))
    }
    Assert-Preview ((Get-PreviewHash (Join-Path $runtime 'bundle-manifest.json')) -ceq $Build.runtimeManifestFileSha256)
    # Fixed trusted program/mode only. Like the existing build probes, capture
    # checks are post-return; the admitted workflow supplies the finite job bound.
    $output = @(& $controller verify 2>&1)
    $exit = $LASTEXITCODE
    $text = $output -join "`n"
    Assert-Preview ($exit -eq 0 -and [Text.Encoding]::UTF8.GetByteCount($text) -le 256KB)
    $document = ConvertFrom-PreviewJson ([Text.Encoding]::UTF8.GetBytes($text))
    Assert-Preview ($document.schemaVersion -ceq 'migration-stream/assisted-local-runtime/cli/v1' -and $document.ok -is [bool] -and $document.ok)
    $v = $document.result
    Assert-Preview ($v.sourceState -ceq 'CLEAN' -and $v.productCommit -ceq $Build.sourceCommit -and $v.sourceSnapshotSha256 -ceq $Build.sourceSnapshotSha256)
    Assert-Preview ($v.target.os -ceq 'windows' -and $v.target.architecture -ceq 'x64' -and $v.target.targetTriple -ceq $Build.target)
    foreach ($entry in @(@('manifestSha256','runtimeManifestSha256'),@('launcherSha256','runtimeLauncherSha256'),@('runtimeControllerSha256','runtimeControllerSha256'),@('integrityCliSha256','integrityCliSha256'),@('workbenchSha256','workbenchSha256'))) {
        Assert-Preview ($v.($entry[0]) -ceq $Build.($entry[1]))
    }
    Assert-Preview ($v.memberCount -eq $Build.runtimeMemberCount -and $v.totalMemberBytes -eq $Build.runtimeMemberBytes)
    return Get-PreviewVerificationIdentity $v
}
function Read-PreviewBlock([IO.Stream]$Stream, [long]$Offset, [int]$Count) {
    Assert-Preview ($Offset -ge 0 -and $Count -ge 0 -and $Count -le 8MB -and $Offset + $Count -le $Stream.Length)
    $Stream.Position = $Offset
    $buffer = [byte[]]::new($Count)
    $read = 0
    while ($read -lt $Count) {
        $n = $Stream.Read($buffer, $read, $Count - $read)
        Assert-Preview ($n -gt 0)
        $read += $n
    }
    return ,$buffer
}
function Assert-PreviewZipStructure([IO.Stream]$Stream) {
    Assert-Preview ($Stream.Length -ge 22 -and $Stream.Length -le $script:PreviewMaximumArchive)
    $end = Read-PreviewBlock $Stream ($Stream.Length - 22) 22
    Assert-Preview ([BitConverter]::ToUInt32($end,0) -eq 0x06054b50 -and [BitConverter]::ToUInt16($end,20) -eq 0)
    $count = [BitConverter]::ToUInt16($end,10)
    $size = [BitConverter]::ToUInt32($end,12)
    $offset = [BitConverter]::ToUInt32($end,16)
    Assert-Preview ([BitConverter]::ToUInt16($end,4) -eq 0 -and [BitConverter]::ToUInt16($end,6) -eq 0 -and [BitConverter]::ToUInt16($end,8) -eq $count)
    Assert-Preview ($count -ge 4 -and $count -le ($script:PreviewMaximumEntries + 1) -and $size -le 8MB -and [long]$offset + $size -eq $Stream.Length - 22)
    $central = Read-PreviewBlock $Stream $offset ([int]$size)
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    [long]$expanded = 0
    [long]$nextLocal = 0
    $position = 0
    for ($i=0; $i -lt $count; $i++) {
        Assert-Preview ($position + 46 -le $central.Length -and [BitConverter]::ToUInt32($central,$position) -eq 0x02014b50)
        $flags = [BitConverter]::ToUInt16($central,$position+8)
        $method = [BitConverter]::ToUInt16($central,$position+10)
        $compressed = [BitConverter]::ToUInt32($central,$position+20)
        $length = [BitConverter]::ToUInt32($central,$position+24)
        $nameLength = [BitConverter]::ToUInt16($central,$position+28)
        $extra = [BitConverter]::ToUInt16($central,$position+30)
        $comment = [BitConverter]::ToUInt16($central,$position+32)
        $attributes = [BitConverter]::ToUInt32($central,$position+38)
        $local = [BitConverter]::ToUInt32($central,$position+42)
        Assert-Preview (($flags -band 0xF7FF) -eq 0 -and $method -eq 0 -and $compressed -eq $length -and $length -le $script:PreviewMaximumMember)
        Assert-Preview ($extra -eq 0 -and $comment -eq 0 -and [BitConverter]::ToUInt16($central,$position+34) -eq 0 -and $nameLength -ge 1 -and $nameLength -le 1024 -and $position + 46 + $nameLength -le $central.Length)
        $nameBytes = $central[($position+46)..($position+45+$nameLength)]
        Assert-Preview (@($nameBytes | Where-Object { $_ -lt 32 -or $_ -gt 126 }).Count -eq 0)
        $name = [Text.Encoding]::ASCII.GetString([byte[]]$nameBytes)
        Assert-PreviewName $name
        Assert-Preview ($seen.Add($name.TrimEnd('/')) -and ($name -ceq $script:PreviewReceiptName -or $name.StartsWith('engineering-layout/',[StringComparison]::Ordinal)))
        $type = ($attributes -shr 16) -band 0xF000
        $directory = $name.EndsWith('/')
        Assert-Preview (($attributes -band 0x400) -eq 0 -and ($type -eq 0 -or ($directory -and $type -eq 0x4000) -or (-not $directory -and $type -eq 0x8000)))
        if ($directory) { Assert-Preview ($length -eq 0) }
        if ($name -ceq $script:PreviewReceiptName) { Assert-Preview ($length -le $script:PreviewMaximumReceipt) }
        $expanded += $length
        Assert-Preview ($expanded -le $script:PreviewMaximumExpanded)
        Assert-Preview ($local -eq $nextLocal)
        $header = Read-PreviewBlock $Stream $local 30
        Assert-Preview ([BitConverter]::ToUInt32($header,0) -eq 0x04034b50 -and [BitConverter]::ToUInt16($header,6) -eq $flags -and [BitConverter]::ToUInt16($header,8) -eq $method)
        Assert-Preview ([BitConverter]::ToUInt32($header,14) -eq [BitConverter]::ToUInt32($central,$position+16) -and [BitConverter]::ToUInt32($header,18) -eq $compressed -and [BitConverter]::ToUInt32($header,22) -eq $length -and [BitConverter]::ToUInt16($header,26) -eq $nameLength -and [BitConverter]::ToUInt16($header,28) -eq 0)
        $localName = Read-PreviewBlock $Stream ($local+30) $nameLength
        Assert-Preview ([Text.Encoding]::ASCII.GetString($localName) -ceq $name)
        $nextLocal = [long]$local + 30 + $nameLength + $compressed
        Assert-Preview ($nextLocal -le $offset)
        $position += 46 + $nameLength
    }
    Assert-Preview ($position -eq $central.Length -and $nextLocal -eq $offset)
    $Stream.Position = 0
    return $count
}
function New-PreviewArchiveCore([string]$Root, [object]$Build, [string]$Output, [scriptblock]$Validate) {
    Assert-PreviewNative (Split-Path -Parent $Output) $true
    Assert-Preview (-not $Output.StartsWith($Root + '\',[StringComparison]::OrdinalIgnoreCase))
    $rows = Get-PreviewRows $Root
    $runtime = & $Validate $Root $Build
    $receipt = [ordered]@{ schemaVersion='migration-stream/windows-engineering-preview/v1'; build=$Build; runtime=$runtime; entries=$rows }
    $receiptBytes = [Text.UTF8Encoding]::new($false).GetBytes((ConvertTo-PreviewStableJson $receipt))
    Assert-Preview ($receiptBytes.Length -le $script:PreviewMaximumReceipt)
    $stream = [IO.File]::Open($Output,[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream,[IO.Compression.ZipArchiveMode]::Create,$true)
        try {
            foreach ($row in $rows) {
                Assert-Preview ($row.path -is [string])
                $name = 'engineering-layout/' + $row.path
                if ($row.type -eq 'directory' -and -not $name.EndsWith('/')) { $name += '/' }
                $entry = $zip.CreateEntry($name,[IO.Compression.CompressionLevel]::NoCompression)
                $entry.LastWriteTime = [DateTimeOffset]::new(1980,1,1,0,0,0,[TimeSpan]::Zero)
                $entry.ExternalAttributes = if ($row.type -eq 'directory') { 16 } else { 0 }
                $target = $entry.Open()
                try {
                    if ($row.type -eq 'file') {
                        $source = [IO.File]::Open((Join-Path $Root $row.path),[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
                        try { $bytes = Read-PreviewBytes $source $script:PreviewMaximumMember } finally { $source.Dispose() }
                        $memory = [IO.MemoryStream]::new($bytes,$false)
                        try { Assert-Preview ((Get-PreviewStreamHash $memory $script:PreviewMaximumMember) -ceq $row.sha256 -and $bytes.Length -eq $row.bytes) } finally { $memory.Dispose() }
                        $target.Write($bytes,0,$bytes.Length)
                    }
                } finally { $target.Dispose() }
            }
            $entry = $zip.CreateEntry($script:PreviewReceiptName,[IO.Compression.CompressionLevel]::NoCompression)
            $entry.LastWriteTime = [DateTimeOffset]::new(1980,1,1,0,0,0,[TimeSpan]::Zero)
            $entry.ExternalAttributes = 0
            $target = $entry.Open()
            try { $target.Write($receiptBytes,0,$receiptBytes.Length) } finally { $target.Dispose() }
        } finally { $zip.Dispose() }
        $stream.Flush($true)
        Assert-Preview ((ConvertTo-PreviewStableJson (Get-PreviewRows $Root)) -ceq (ConvertTo-PreviewStableJson $rows))
        $null = Assert-PreviewZipStructure $stream
        $stream.Position = 0
        $hash = Get-PreviewStreamHash $stream $script:PreviewMaximumArchive
        return [ordered]@{ schemaVersion='migration-stream/windows-engineering-candidate/v1'; status='CANDIDATE_LOCAL'; archiveSha256=$hash; archiveBytes=$stream.Length; documentLauncherSha256=$Build.documentLauncherSha256; sourceCommit=$Build.sourceCommit }
    } finally { $stream.Dispose() }
}
function Expand-PreviewArchiveCore([string]$InputArchive,[string]$ExpectedHash,[string]$Output,[scriptblock]$Validate) {
    Assert-Preview ($ExpectedHash -cmatch '^[0-9a-f]{64}$')
    Assert-PreviewNative $InputArchive $false
    Assert-PreviewNative (Split-Path -Parent $Output) $true
    Assert-Preview (-not $InputArchive.StartsWith($Output + '\',[StringComparison]::OrdinalIgnoreCase))
    # Do not unblock or silently discard browser marking. This reference route
    # refuses marked input; browser/Explorer extraction remains unqualified.
    if ($IsWindows) {
        try { $zone = [IO.File]::Open($InputArchive + ':Zone.Identifier',[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read) }
        catch [IO.FileNotFoundException] { $zone = $null }
        if ($null -ne $zone) { $zone.Dispose(); throw 'browser-marked candidate is outside this reference route' }
    }
    $stream = [IO.File]::Open($InputArchive,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try {
        Assert-Preview ((Get-PreviewStreamHash $stream $script:PreviewMaximumArchive) -ceq $ExpectedHash)
        $count = Assert-PreviewZipStructure $stream
        $zip = [IO.Compression.ZipArchive]::new($stream,[IO.Compression.ZipArchiveMode]::Read,$true)
        try {
            Assert-Preview ($zip.Entries.Count -eq $count)
            $receiptEntry = $zip.GetEntry($script:PreviewReceiptName)
            Assert-Preview ($null -ne $receiptEntry)
            $input = $receiptEntry.Open()
            try { $receipt = ConvertFrom-PreviewJson (Read-PreviewBytes $input $script:PreviewMaximumReceipt) } finally { $input.Dispose() }
            Assert-Preview ($receipt.schemaVersion -ceq 'migration-stream/windows-engineering-preview/v1' -and $receipt.entries.Count -ge 3 -and $receipt.entries.Count -le $script:PreviewMaximumEntries)
            $expected = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
            foreach ($row in $receipt.entries) {
                Assert-Preview ($row.path -is [string])
                $name = 'engineering-layout/' + $row.path
                if ($row.type -eq 'directory' -and -not $name.EndsWith('/')) { $name += '/' }
                Assert-PreviewName $name
                Assert-Preview ($row.type -cin @('file','directory') -and -not $expected.ContainsKey($name))
                Assert-Preview ($row.bytes -is [long] -or $row.bytes -is [int])
                Assert-Preview ($row.bytes -ge 0 -and $row.bytes -le $script:PreviewMaximumMember)
                if ($row.type -eq 'file') { Assert-Preview ($row.sha256 -cmatch '^[0-9a-f]{64}$') }
                else { Assert-Preview ($row.bytes -eq 0 -and $null -eq $row.sha256) }
                $expected.Add($name,$row)
            }
            Assert-Preview ($expected.Count + 1 -eq $zip.Entries.Count)
            $stage = Join-Path (Split-Path -Parent $Output) ('.preview-extract-' + [Guid]::NewGuid())
            New-Item -ItemType Directory -Path $stage | Out-Null
            foreach ($entry in $zip.Entries) {
                if ($entry.FullName -ceq $script:PreviewReceiptName) { continue }
                Assert-Preview ($expected.ContainsKey($entry.FullName))
                $row = $expected[$entry.FullName]
                $path = Join-Path $stage ($entry.FullName.TrimEnd('/'))
                if ($row.type -eq 'directory') {
                    Assert-Preview ($entry.FullName.EndsWith('/') -and $entry.Length -eq 0)
                    New-Item -ItemType Directory -Path $path | Out-Null
                } else {
                    Assert-Preview (-not $entry.FullName.EndsWith('/') -and $entry.Length -eq $row.bytes)
                    Assert-PreviewNative (Split-Path -Parent $path) $true
                    $input = $entry.Open()
                    try { $bytes = Read-PreviewBytes $input $script:PreviewMaximumMember } finally { $input.Dispose() }
                    $memory = [IO.MemoryStream]::new($bytes,$false)
                    try { Assert-Preview ($bytes.Length -eq $row.bytes -and (Get-PreviewStreamHash $memory $script:PreviewMaximumMember) -ceq $row.sha256) } finally { $memory.Dispose() }
                    $file = [IO.File]::Open($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
                    try { $file.Write($bytes,0,$bytes.Length); $file.Flush($true) } finally { $file.Dispose() }
                }
            }
            $layout = Join-Path $stage 'engineering-layout'
            Assert-Preview ((ConvertTo-PreviewStableJson (Get-PreviewRows $layout)) -ceq (ConvertTo-PreviewStableJson $receipt.entries))
            # Same-parent no-replace publication; no recursive cleanup on failure.
            [IO.Directory]::Move($stage,$Output)
            $layout = Join-Path $Output 'engineering-layout'
            Assert-Preview ((ConvertTo-PreviewStableJson (& $Validate $layout $receipt.build)) -ceq (ConvertTo-PreviewStableJson $receipt.runtime))
            return [ordered]@{ schemaVersion='migration-stream/windows-engineering-candidate-replay/v1'; status='CANDIDATE_REPLAY_PASSED'; archiveSha256=$ExpectedHash; sourceCommit=$receipt.build.sourceCommit; documentLauncherSha256=$receipt.build.documentLauncherSha256; runtimeManifestSha256=$receipt.build.runtimeManifestSha256 }
        } finally { $zip.Dispose() }
    } finally { $stream.Dispose() }
}

if ($MyInvocation.InvocationName -ne '.') {
    try {
        Assert-Preview $IsWindows
        $archivePath = Get-PreviewFullPath $Archive
        if ($Operation -eq 'Pack') {
            $layoutPath = Get-PreviewFullPath $Layout
            $build = Select-PreviewBuildReceipt (Read-PreviewJsonFile (Get-PreviewFullPath $BuildReceipt))
            $result = New-PreviewArchiveCore $layoutPath $build $archivePath ${function:Invoke-PreviewRuntimeVerification}
        } elseif ($Operation -eq 'Extract') {
            $result = Expand-PreviewArchiveCore $archivePath $ExpectedArchiveSha256 (Get-PreviewFullPath $Destination) ${function:Invoke-PreviewRuntimeVerification}
        } else { throw 'preview operation required' }
        ConvertTo-PreviewStableJson $result
    } catch { Write-Output 'Engineering candidate operation refused or failed'; exit 1 }
}
