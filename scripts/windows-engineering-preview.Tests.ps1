# Tiny synthetic stored ZIP fixtures only. Never call the production verifier.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-engineering-preview.ps1')
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('migration-stream-preview-synthetic-' + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $testRoot | Out-Null
$script:testCount = 0
function Expect-Refusal([scriptblock]$Body) {
    $refused = $false
    try { & $Body | Out-Null } catch { $refused = $true }
    Assert-Preview $refused
}
function Test-Case([string]$Name,[scriptblock]$Body) {
    & $Body
    $script:testCount++
    Write-Output ('PASS synthetic preview case=' + $Name)
}
$fakeValidator = {
    param($root,$build)
    Assert-Preview ((Get-PreviewHash (Join-Path $root 'migration-stream-document-open.exe')) -ceq $build.documentLauncherSha256)
    return [ordered]@{ manifestSha256=$build.runtimeManifestSha256; sourceCommit=$build.sourceCommit; fixture='no product process' }
}
try {
    $layout = Join-Path $testRoot 'synthetic-layout'
    New-Item -ItemType Directory -Path (Join-Path $layout 'Runtime') | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $layout 'migration-stream-document-open.exe'),[byte[]](1,2,3))
    [IO.File]::WriteAllBytes((Join-Path $layout 'Runtime/bundle-manifest.json'),[Text.Encoding]::UTF8.GetBytes('{"synthetic":true}'))
    $build = [ordered]@{ documentLauncherSha256=(Get-PreviewHash (Join-Path $layout 'migration-stream-document-open.exe')); runtimeManifestSha256=('a'*64); sourceCommit=('b'*40) }
    $archive = Join-Path $testRoot 'candidate.zip'
    $candidate = New-PreviewArchiveCore $layout $build $archive $fakeValidator
    Test-Case 'production-core-roundtrip' {
        $result = Expand-PreviewArchiveCore $archive $candidate.archiveSha256 (Join-Path $testRoot 'roundtrip') $fakeValidator
        Assert-Preview ($result.status -ceq 'CANDIDATE_REPLAY_PASSED')
        Assert-Preview ((ConvertTo-PreviewStableJson (Get-PreviewRows $layout)) -ceq (ConvertTo-PreviewStableJson (Get-PreviewRows (Join-Path $testRoot 'roundtrip/engineering-layout'))))
    }
    Test-Case 'external-hash-and-tamper' {
        Expect-Refusal { Expand-PreviewArchiveCore $archive ('0'*64) (Join-Path $testRoot 'wrong-hash') $fakeValidator }
        $tampered = Join-Path $testRoot 'tampered.zip'
        [IO.File]::Copy($archive,$tampered)
        $bytes = [IO.File]::ReadAllBytes($tampered); $bytes[40] = $bytes[40] -bxor 1; [IO.File]::WriteAllBytes($tampered,$bytes)
        Expect-Refusal { Expand-PreviewArchiveCore $tampered $candidate.archiveSha256 (Join-Path $testRoot 'tampered-output') $fakeValidator }
    }
    Test-Case 'no-overwrite-and-overlap' {
        Expect-Refusal { New-PreviewArchiveCore $layout $build $archive $fakeValidator }
        Expect-Refusal { New-PreviewArchiveCore $layout $build (Join-Path $layout 'overlap.zip') $fakeValidator }
        $existing = Join-Path $testRoot 'existing'; New-Item -ItemType Directory -Path $existing | Out-Null
        [IO.File]::WriteAllText((Join-Path $existing 'sentinel'),'preserve')
        Expect-Refusal { Expand-PreviewArchiveCore $archive $candidate.archiveSha256 $existing $fakeValidator }
        Assert-Preview ([IO.File]::ReadAllText((Join-Path $existing 'sentinel')) -ceq 'preserve')
    }
    Test-Case 'refusing-staged-validator-never-publishes' {
        $destination=Join-Path $testRoot 'refused-before-publication'
        $script:refusingValidatorCalls=0
        $refusingValidator={
            param($root,$build)
            $script:refusingValidatorCalls++
            throw 'synthetic validation refusal'
        }
        Expect-Refusal { Expand-PreviewArchiveCore $archive $candidate.archiveSha256 $destination $refusingValidator }
        Assert-Preview ($script:refusingValidatorCalls -eq 1)
        Assert-Preview (-not [IO.Directory]::Exists($destination) -and -not [IO.File]::Exists($destination))
    }
    Test-Case 'namespace-and-depth-refusals' {
        foreach ($name in @('../x','/x','C:/x','//server/x','x\y','x:stream','CON','x/NUL.txt','x.','x//y',('x/'*67+'y'))) {
            Expect-Refusal { Assert-PreviewName $name }
        }
    }
    Test-Case 'json-duplicates-and-caps' {
        Expect-Refusal { ConvertFrom-PreviewJson ([Text.Encoding]::UTF8.GetBytes('{"x":1,"X":2}')) }
        Expect-Refusal { ConvertFrom-PreviewJson ([byte[]]::new(1MB+1)) }
    }
    Test-Case 'central-count-and-expansion-bounds' {
        $bad = Join-Path $testRoot 'bad-count.zip'; $bytes=[IO.File]::ReadAllBytes($archive)
        [BitConverter]::GetBytes([uint16]60000).CopyTo($bytes,$bytes.Length-14)
        [BitConverter]::GetBytes([uint16]60000).CopyTo($bytes,$bytes.Length-12)
        [IO.File]::WriteAllBytes($bad,$bytes)
        $stream=[IO.File]::OpenRead($bad)
        try { Expect-Refusal { Assert-PreviewZipStructure $stream } } finally { $stream.Dispose() }
    }
    Test-Case 'compressed-zip-bomb-format-refusal' {
        $bad=Join-Path $testRoot 'deflated.zip'; $file=[IO.File]::Open($bad,[IO.FileMode]::CreateNew)
        $zip=[IO.Compression.ZipArchive]::new($file,[IO.Compression.ZipArchiveMode]::Create,$true)
        foreach ($name in @('engineering-layout/','engineering-layout/Runtime/','engineering-layout/x','preview-receipt.json')) {
            $entry=$zip.CreateEntry($name,[IO.Compression.CompressionLevel]::Optimal)
            $body=$entry.Open(); $body.Write([byte[]]::new(1024),0,1024); $body.Dispose()
        }
        $zip.Dispose();$file.Dispose()
        $stream=[IO.File]::OpenRead($bad)
        try { Expect-Refusal { Assert-PreviewZipStructure $stream } } finally { $stream.Dispose() }
    }
    Test-Case 'duplicate-case-path-and-special-entry-refusals' {
        foreach ($badName in @('engineering-layout/../escape','engineering-layout/C:ads','engineering-layout/CON','engineering-layout/MIGRATION-STREAM-DOCUMENT-OPEN.EXE','engineering-layout/Runtime/')) {
            $bad = Join-Path $testRoot ('unsafe-' + [Guid]::NewGuid() + '.zip')
            $file=[IO.File]::Open($bad,[IO.FileMode]::CreateNew)
            $zip=[IO.Compression.ZipArchive]::new($file,[IO.Compression.ZipArchiveMode]::Create,$true)
            foreach ($name in @('engineering-layout/','engineering-layout/Runtime/','engineering-layout/migration-stream-document-open.exe',$badName,'preview-receipt.json')) {
                $entry=$zip.CreateEntry($name,[IO.Compression.CompressionLevel]::NoCompression)
                $entry.ExternalAttributes=0
                $body=$entry.Open();$body.Dispose()
            }
            $zip.Dispose();$file.Dispose()
            $stream=[IO.File]::OpenRead($bad)
            try { Expect-Refusal { Assert-PreviewZipStructure $stream } } finally { $stream.Dispose() }
        }
        $bad=Join-Path $testRoot 'special.zip';$bytes=[IO.File]::ReadAllBytes($archive)
        $centralOffset=[BitConverter]::ToUInt32($bytes,$bytes.Length-6)
        [BitConverter]::GetBytes([uint32]0xA0000000L).CopyTo($bytes,$centralOffset+38)
        [IO.File]::WriteAllBytes($bad,$bytes)
        $stream=[IO.File]::OpenRead($bad)
        try { Expect-Refusal { Assert-PreviewZipStructure $stream } } finally { $stream.Dispose() }
    }
    Test-Case 'member-total-and-depth-bounds' {
        $old=$script:PreviewMaximumMember
        try { $script:PreviewMaximumMember=2; Expect-Refusal { Get-PreviewRows $layout } }
        finally { $script:PreviewMaximumMember=$old }
        $old=$script:PreviewMaximumExpanded
        $stream=[IO.File]::OpenRead($archive)
        try { $script:PreviewMaximumExpanded=2; Expect-Refusal { Assert-PreviewZipStructure $stream } }
        finally { $script:PreviewMaximumExpanded=$old;$stream.Dispose() }
    }
    Test-Case 'readonly-is-not-a-byte-authority' {
        $file=Get-Item -LiteralPath (Join-Path $layout 'Runtime/bundle-manifest.json')
        $file.IsReadOnly=$true
        try {
            Assert-Preview ((Get-PreviewHash $file.FullName) -ceq (Get-PreviewHash (Join-Path $testRoot 'roundtrip/engineering-layout/Runtime/bundle-manifest.json')))
        } finally { $file.IsReadOnly=$false }
    }
    Test-Case 'hardlinks-refused' {
        $target=Join-Path $layout 'migration-stream-document-open.exe'
        $alias=Join-Path $testRoot 'hardlink.exe'
        New-Item -ItemType HardLink -Path $alias -Target $target | Out-Null
        try { Expect-Refusal { Get-PreviewRows $layout } } finally { [IO.File]::Delete($alias) }
    }
    Test-Case 'ordinary-drive-paths-and-ads-output-refusal' {
        Assert-Preview ((Get-PreviewFullPath 'C:\') -ceq 'C:\')
        Assert-Preview ((Get-PreviewFullPath 'C:\engineering Ω space\candidate.zip') -ceq 'C:\engineering Ω space\candidate.zip')
        foreach ($bad in @('C:\x:stream','C:\CON\x','C:\CON .txt','C:\x.\y','C:\x \y','C:\x\\y','C:/x','C:\x\..\y','\\?\C:\x',('C:\x'+[char]1+'y'))) {
            Expect-Refusal { Get-PreviewFullPath $bad }
        }
        $sentinel=Join-Path $testRoot 'ordinary-sentinel'
        [IO.File]::WriteAllText($sentinel,'preserve')
        $ads=$sentinel+':preview.zip'
        Expect-Refusal { New-PreviewArchiveCore $layout $build $ads $fakeValidator }
        Assert-Preview ([IO.File]::ReadAllText($sentinel) -ceq 'preserve')
        Assert-Preview (-not [IO.File]::Exists($ads))
    }
    Test-Case 'marked-source-refused-before-archive' {
        foreach ($relative in @('migration-stream-document-open.exe','Runtime/bundle-manifest.json')) {
            $source=Join-Path $layout $relative
            $marker=$source+':Zone.Identifier'
            [IO.File]::WriteAllText($marker,"[ZoneTransfer]`r`nZoneId=3")
            $newArchive=Join-Path $testRoot ('marked-source-'+[Guid]::NewGuid()+'.zip')
            try {
                Expect-Refusal { New-PreviewArchiveCore $layout $build $newArchive $fakeValidator }
                Assert-Preview (-not [IO.File]::Exists($newArchive))
                Assert-Preview ([IO.File]::Exists($marker))
            } finally { [IO.File]::Delete($marker) }
        }
    }
    Test-Case 'browser-marker-not-unblocked' {
        $marked=Join-Path $testRoot 'marked.zip';[IO.File]::Copy($archive,$marked)
        [IO.File]::WriteAllText($marked+':Zone.Identifier',"[ZoneTransfer]`r`nZoneId=3")
        Expect-Refusal { Expand-PreviewArchiveCore $marked $candidate.archiveSha256 (Join-Path $testRoot 'marked-output') $fakeValidator }
        Assert-Preview ([IO.File]::Exists($marked+':Zone.Identifier'))
    }
    Write-Output ('Synthetic preview cases passed=' + $script:testCount + '; no product process or publication')
} finally {
    # Only the test's freshly created temporary root; no shared root cleanup.
    Remove-Item -LiteralPath $testRoot -Recurse -Force
}
