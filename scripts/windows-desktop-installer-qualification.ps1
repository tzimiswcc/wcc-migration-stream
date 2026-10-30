[CmdletBinding()]
param([Parameter(Mandatory)][ValidateSet('Native','AdmitTransfers','Combine')][string]$Operation)
$desktopOperation = $Operation
$ErrorActionPreference='Stop'
$PSNativeCommandUseErrorActionPreference=$false
function Require-Desktop([bool]$Value) { if (-not $Value) { throw 'desktop qualification refused' } }
function Hash-Desktop([string]$Path) {
  $item=Get-Item -LiteralPath $Path -Force
  Require-Desktop (-not $item.PSIsContainer -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))
  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Output-Desktop([string]$Name,[string]$Value) {
  Require-Desktop ($Name -cmatch '^[a-z_]+$' -and $Value -cmatch '^[a-zA-Z0-9._-]{1,128}$')
  [IO.File]::AppendAllText($env:GITHUB_OUTPUT,$Name+'='+$Value+"`n")
}
try {
  Require-Desktop ($env:PUBLISH_NATIVE_TRANSFERS -ceq 'true')
  Require-Desktop ($env:GITHUB_RUN_ATTEMPT -ceq '1' -and $env:GITHUB_RUN_ID -cmatch '^[0-9]{1,20}$')
  Require-Desktop ($env:EXPECTED_SOURCE_COMMIT -cmatch '^[0-9a-f]{40}$' -and $env:GITHUB_SHA -ceq $env:EXPECTED_SOURCE_COMMIT)
  Require-Desktop ((@(& git rev-parse HEAD) -join '').Trim() -ceq $env:GITHUB_SHA -and $LASTEXITCODE -eq 0)
  $desktopTree=(@(& git rev-parse 'HEAD^{tree}') -join '').Trim()
  Require-Desktop ($LASTEXITCODE -eq 0 -and $desktopTree -cmatch '^[0-9a-f]{40}$')
  Require-Desktop (@(& git status --porcelain=v1 --untracked-files=all).Count -eq 0 -and $LASTEXITCODE -eq 0)
  Require-Desktop (Test-Path -LiteralPath './scripts/package-windows-desktop.py' -PathType Leaf)
  if ($desktopOperation -ceq 'Native') {
    # Intentional same-scope composition retains only the existing driver's
    # freshly created output paths. Its guards, build recipe and probes stay intact.
    Require-Desktop ($env:PUBLISH_ENGINEERING_PREVIEW -ceq 'true')
    . ./scripts/windows-engineering-qualification.ps1
    Require-Desktop ($qualificationMetadata.status -ceq 'PASSED' -and $targetTuple.architecture -ceq $env:QUALIFICATION_ARCHITECTURE)
    $transfer=Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-transfer-'+$env:QUALIFICATION_ARCHITECTURE+'-'+$env:GITHUB_RUN_ID)
    New-Item -ItemType Directory -Path $transfer | Out-Null
    $archiveHash=Hash-Desktop $candidateArchive
    Copy-Item -LiteralPath $candidateArchive -Destination (Join-Path $transfer 'native-layout.zip') -ErrorAction Stop
    Copy-Item -LiteralPath $evidencePath -Destination (Join-Path $transfer 'qualification.json') -ErrorAction Stop
    Require-Desktop ((Hash-Desktop (Join-Path $transfer 'native-layout.zip')) -ceq $archiveHash)
    $receiptHash=Hash-Desktop $evidencePath
    Require-Desktop ((Hash-Desktop (Join-Path $transfer 'qualification.json')) -ceq $receiptHash)
    Output-Desktop 'archive_sha' $archiveHash
    Output-Desktop 'receipt_sha' $receiptHash
    Output-Desktop 'source_tree' $desktopTree
    Output-Desktop 'snapshot_sha' $qualificationMetadata.sourceSnapshotSha256
    Write-Output ('DESKTOP_NATIVE_TRANSFER_READY architecture='+$targetTuple.architecture+' readerAccessible=true retentionDays=1')
  } else {
    . ./scripts/windows-engineering-preview.ps1
    $tuple=Get-PreviewTarget 'x64'
    Assert-PreviewNativeHost $tuple $env:PROCESSOR_ARCHITECTURE ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) ([Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString())
    Require-Desktop ($env:X64_TREE -ceq $desktopTree -and $env:ARM64_TREE -ceq $desktopTree -and $env:X64_SNAPSHOT -ceq $env:ARM64_SNAPSHOT -and $env:X64_SNAPSHOT -cmatch '^[0-9a-f]{64}$')
    foreach ($arch in @('X64','ARM64')) {
      Require-Desktop ([Environment]::GetEnvironmentVariable($arch+'_ARTIFACT_ID') -cmatch '^[0-9]{1,20}$')
      foreach ($field in @('ARTIFACT_DIGEST','ARCHIVE_SHA','RECEIPT_SHA')) { Require-Desktop ([Environment]::GetEnvironmentVariable($arch+'_'+$field) -cmatch '^[0-9a-f]{64}$') }
    }
    Require-Desktop ($env:X64_ARTIFACT_ID -cne $env:ARM64_ARTIFACT_ID)
    if ($desktopOperation -ceq 'AdmitTransfers') {
      Write-Output 'DESKTOP_TRANSFER_IDS_ADMITTED exact immutable IDs only'
      return
    }
    Require-Desktop ($env:RUNNER_TEMP -cmatch '^[A-Za-z]:\\')
    $drive=Get-PSDrive -Name ([IO.Path]::GetPathRoot($env:RUNNER_TEMP).Substring(0,1))
    $memory=Get-CimInstance -ClassName Win32_OperatingSystem
    Require-Desktop ($drive.Free -ge 12GB -and ([long]$memory.TotalVisibleMemorySize * 1KB) -ge 8GB)
    $parent=Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-combine-'+$env:GITHUB_RUN_ID)
    New-Item -ItemType Directory -Path $parent | Out-Null
    $python=(Get-Command python -CommandType Application -ErrorAction Stop | Select-Object -First 1).Path
    $pythonVersion=@(& $python -c 'import sys; assert sys.version_info[:2]>=(3,11); print("%d.%d.%d"%sys.version_info[:3])')
    Require-Desktop ($LASTEXITCODE -eq 0 -and $pythonVersion.Count -eq 1 -and $pythonVersion[0] -cmatch '^3\.[0-9]{1,2}\.[0-9]{1,3}$')
    foreach ($arch in @('x64','arm64')) {
      $downloadInput=Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-download-'+$arch+'-'+$env:GITHUB_RUN_ID)
      $items=@(Get-ChildItem -LiteralPath $downloadInput -Force)
      Require-Desktop ($items.Count -eq 2 -and @($items | Where-Object { $_.PSIsContainer -or ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) }).Count -eq 0)
      Require-Desktop (@($items.Name | Where-Object { $_ -cnotin @('native-layout.zip','qualification.json') }).Count -eq 0)
      $toolArguments=@('-B','scripts/windows-desktop-transfer.py','--archive',(Join-Path $downloadInput 'native-layout.zip'),'--archive-sha',[Environment]::GetEnvironmentVariable($arch.ToUpperInvariant()+'_ARCHIVE_SHA'),'--receipt',(Join-Path $downloadInput 'qualification.json'),'--receipt-sha',[Environment]::GetEnvironmentVariable($arch.ToUpperInvariant()+'_RECEIPT_SHA'),'--destination',(Join-Path $parent $arch),'--architecture',$arch,'--source-commit',$env:GITHUB_SHA,'--source-tree',$desktopTree)
      $result=@(& $python @toolArguments)
      Require-Desktop ($LASTEXITCODE -eq 0 -and ($result -join '').Length -le 8192)
      $proof=($result -join "`n") | ConvertFrom-Json
      Require-Desktop ($proof.status -ceq 'PASSIVE_TRANSFER_ONLY' -and $proof.architecture -ceq $arch -and $proof.sourceSnapshotSha256 -ceq $env:X64_SNAPSHOT)
    }
    # One explicit closed portable preparation; never use the root SubStart
    # launcher, search PATH/ProgramFiles or install a compiler globally.
    $preparedDirectory=Join-Path $parent 'nsis-tool'
    $preparedText=@(& $python -B scripts/prepare-nsis-tool.py --destination $preparedDirectory)
    Require-Desktop ($LASTEXITCODE -eq 0 -and ($preparedText -join '').Length -le 16384)
    $prepared=($preparedText -join "`n") | ConvertFrom-Json
    Require-Desktop ($prepared.version -is [string] -and $prepared.version -ceq '3.13' -and $prepared.compilerSha256 -is [string] -and $prepared.compilerSha256 -cmatch '^[0-9a-f]{64}$')
    $nsisRoot=Join-Path $preparedDirectory 'nsis-3.13'
    $makensis=Join-Path $nsisRoot 'Bin/makensis.exe'
    Require-Desktop ($prepared.compiler -is [string] -and $prepared.compiler -ceq $makensis -and $prepared.resourceRoot -is [string] -and $prepared.resourceRoot -ceq $nsisRoot)
    $nsisHash=Hash-Desktop $makensis
    Require-Desktop ($nsisHash -ceq $prepared.compilerSha256)
    $resourceHashes=$prepared.resourceSha256
    # The pinned direct compiler has no RT_VERSION. Release version authority
    # is the validated archive receipt; PE metadata is optional evidence only.
    $versionInfo=[Diagnostics.FileVersionInfo]::GetVersionInfo($makensis)
    $nsisFileVersion=$null
    if ($versionInfo.FileVersion -is [string] -and $versionInfo.FileVersion -cmatch '^[0-9., ]{1,40}$') {
      $nsisFileVersion=$versionInfo.FileVersion
    }
    $product=(Get-Content -Raw -LiteralPath Cargo.toml | Select-String -Pattern '(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+)"$').Matches
    Require-Desktop ($product.Count -eq 1)
    $version=$product[0].Groups[1].Value
    $toolArguments=@('-B','scripts/package-windows-desktop.py','--source-commit',$env:GITHUB_SHA,'--source-tree',$desktopTree,'--source-snapshot-sha256',$env:X64_SNAPSHOT,'--product-version',$version,'--makensis',$makensis,'--makensis-sha256',$nsisHash,'--output-directory',(Join-Path $parent 'installer'))
    foreach ($arch in @('x64','arm64')) { $toolArguments+=@(('--'+$arch+'-layout'),(Join-Path $parent ($arch+'/engineering-layout')),('--'+$arch+'-receipt'),(Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-download-'+$arch+'-'+$env:GITHUB_RUN_ID+'/qualification.json')),('--'+$arch+'-receipt-sha256'),[Environment]::GetEnvironmentVariable($arch.ToUpperInvariant()+'_RECEIPT_SHA')) }
    $result=@(& $python @toolArguments)
    Require-Desktop ($LASTEXITCODE -eq 0 -and ($result -join '').Length -le 8192)
    $proof=($result -join "`n") | ConvertFrom-Json
    Require-Desktop ($proof.ok -is [bool] -and $proof.ok -and $proof.installer.sha256 -cmatch '^[0-9a-f]{64}$')
    Require-Desktop ((Hash-Desktop (Join-Path $parent 'installer/Migration Stream Setup.exe')) -ceq $proof.installer.sha256)
    $replayedTool=@(& $python -B scripts/prepare-nsis-tool.py --validate-prepared $preparedDirectory)
    Require-Desktop ($LASTEXITCODE -eq 0 -and ($replayedTool -join '').Length -le 16384 -and ($replayedTool -join '') -ceq ($preparedText -join ''))
    Require-Desktop (@(& git status --porcelain=v1 --untracked-files=all).Count -eq 0 -and $LASTEXITCODE -eq 0)
    $evidence=[ordered]@{schemaVersion='migration-stream/windows-desktop-installer-qualification/v1';status='COMPILED_PASSIVE_BYTES_ONLY';sourceCommit=$env:GITHUB_SHA;sourceTree=$desktopTree;sourceSnapshotSha256=$env:X64_SNAPSHOT;runId=$env:GITHUB_RUN_ID;runAttempt=1;pythonVersion=$pythonVersion[0];pythonSha256=(Hash-Desktop $python);nsisArchiveSha256=$prepared.archiveSha256;nsisPreparationReceiptSha256=$prepared.preparedReceiptSha256;nsisExeSha256=$nsisHash;nsisVersion=$prepared.version;nsisVersionEvidence='PINNED_ARCHIVE_RECEIPT';nsisFileVersion=$nsisFileVersion;nsisResourceSha256=$resourceHashes;x64ArtifactId=$env:X64_ARTIFACT_ID;x64ArtifactDigest=$env:X64_ARTIFACT_DIGEST;arm64ArtifactId=$env:ARM64_ARTIFACT_ID;arm64ArtifactDigest=$env:ARM64_ARTIFACT_DIGEST;installerSha256=$proof.installer.sha256;installerBytes=$proof.installer.bytes;installerExecuted=$false;setupUploaded=$false;transferRetentionDays=1}
    $json=$evidence | ConvertTo-Json -Compress
    Require-Desktop ($json.Length -le 8192)
    Write-Output $json
  }
} catch { Write-Output 'Desktop installer qualification refused; partials and explicitly unqualified transfer artifacts preserved'; exit 1 }
