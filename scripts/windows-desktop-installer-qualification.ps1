[CmdletBinding()]
param([Parameter(Mandatory)][ValidateSet('Native','AdmitTransfers','Combine')][string]$Operation,[switch]$ReuseFixedNativePair)
$desktopOperation = $Operation
$ErrorActionPreference='Stop'
$PSNativeCommandUseErrorActionPreference=$false
# Fixed diagnostics only: never serialize an exception, path, argv or environment.
$script:desktopStage='source-admission'
$script:desktopGuard='G00'
$script:desktopExitCode=$null
$script:desktopFailureCategory='operation'
function Set-DesktopStage([ValidateSet('source-admission','native-build','native-transfer','host-admission','transfer-metadata','resource-path','disk-probe','memory-probe','disk-admission','memory-admission','private-parent','python-selection','python-admission','x64-file-admission','arm64-file-admission','x64-passive-replay','arm64-passive-replay','nsis-preparation','nsis-binding','product-version','installer-compile','postcompile-integrity','final-evidence')][string]$Stage) {
  $script:desktopStage=$Stage
  $script:desktopGuard='G00'
  $script:desktopExitCode=$null
  $script:desktopFailureCategory='operation'
  Write-Output ('DESKTOP_QUALIFICATION stage='+$Stage)
}
function Require-Desktop([bool]$Value,[ValidatePattern('^G[0-9]{2}$')][string]$Guard='G00') {
  if (-not $Value) {
    $script:desktopGuard=$Guard
    $script:desktopFailureCategory=if ($null -ne $script:desktopExitCode -and $script:desktopExitCode -ne 0) { 'native-exit' } else { 'guard' }
    throw 'desktop qualification refused'
  }
}
function Hash-Desktop([string]$Path) {
  $item=Get-Item -LiteralPath $Path -Force
  Require-Desktop (-not $item.PSIsContainer -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) 'G01'
  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Output-Desktop([string]$Name,[string]$Value) {
  Require-Desktop ($Name -cmatch '^[a-z_]+$' -and $Value -cmatch '^[a-zA-Z0-9._-]{1,128}$') 'G02'
  [IO.File]::AppendAllText($env:GITHUB_OUTPUT,$Name+'='+$Value+"`n")
}
try {
  Set-DesktopStage 'source-admission'
  Require-Desktop ($env:PUBLISH_NATIVE_TRANSFERS -ceq 'true') 'G03'
  Require-Desktop ($env:GITHUB_RUN_ATTEMPT -ceq '1' -and $env:GITHUB_RUN_ID -cmatch '^[0-9]{1,20}$') 'G04'
  Require-Desktop ($env:EXPECTED_SOURCE_COMMIT -cmatch '^[0-9a-f]{40}$' -and $env:GITHUB_SHA -ceq $env:EXPECTED_SOURCE_COMMIT) 'G05'
  $desktopHead=@(& git rev-parse HEAD)
  $script:desktopExitCode=$LASTEXITCODE
  Require-Desktop (($desktopHead -join '').Trim() -ceq $env:GITHUB_SHA -and $script:desktopExitCode -eq 0) 'G06'
  $desktopTreeOutput=@(& git rev-parse 'HEAD^{tree}')
  $script:desktopExitCode=$LASTEXITCODE
  $desktopTree=($desktopTreeOutput -join '').Trim()
  Require-Desktop ($script:desktopExitCode -eq 0 -and $desktopTree -cmatch '^[0-9a-f]{40}$') 'G07'
  $desktopStatus=@(& git status --porcelain=v1 --untracked-files=all)
  $script:desktopExitCode=$LASTEXITCODE
  Require-Desktop ($desktopStatus.Count -eq 0 -and $script:desktopExitCode -eq 0) 'G08'
  Require-Desktop (Test-Path -LiteralPath './scripts/package-windows-desktop.py' -PathType Leaf) 'G09'
  # Default path retains same-source behavior. Only the closed reuse workflow
  # can admit old product bytes, with independently recorded tooling provenance.
  $productCommit=$env:GITHUB_SHA
  $productTree=$desktopTree
  $fixedPairAdmission=$null
  if ($ReuseFixedNativePair) {
    Set-DesktopStage 'source-admission'
    Require-Desktop ($desktopOperation -cin @('AdmitTransfers','Combine')) 'G37'
    $admissionPython=(Get-Command python -CommandType Application -ErrorAction Stop | Select-Object -First 1).Path
    $admissionText = @(& $admissionPython -I -B scripts/windows-combine-fixed-inputs.py --admit)
    $script:desktopExitCode=$LASTEXITCODE
    $env:GH_TOKEN=$null
    if ($script:desktopExitCode -ne 0) {
      # Captured stdout is never forwarded. Only closed scalar failure fields
      # can cross the public diagnostic boundary; malformed output stays unknown.
      $safePhase='unknown';$safeCategory='unknown';$safeHttp=$null
      if (($admissionText -join '').Length -le 16384) {
        try {
          $refusal=($admissionText -join "`n") | ConvertFrom-Json
          if ($refusal.status -is [string] -and $refusal.status -ceq 'FIXED_NATIVE_PAIR_REFUSED' -and $refusal.phase -is [string] -and $refusal.phase -cin @('source-inventory','source-ref','source-dirty','source-delta','run-projection','job-projection','artifact-projection','expiry','api-transport','api-status','api-size','api-json','unknown') -and $refusal.category -is [string] -and $refusal.category -cin @('projection','git','deadline','transport','timeout','redirect','http-status','size','json','unknown','token')) {
            $safePhase=$refusal.phase;$safeCategory=$refusal.category
            if (($refusal.httpStatus -is [int] -or $refusal.httpStatus -is [long]) -and $refusal.httpStatus -ge 100 -and $refusal.httpStatus -le 599) { $safeHttp=$refusal.httpStatus }
          }
        } catch { }
      }
      $safeRefusal=[ordered]@{phase=$safePhase;category=$safeCategory;httpStatus=$safeHttp}
      Write-Output ('FIXED_PAIR_ADMISSION_FAILURE '+($safeRefusal | ConvertTo-Json -Compress))
    }
    Require-Desktop ($script:desktopExitCode -eq 0 -and ($admissionText -join '').Length -le 16384) 'G38'
    $fixedPairAdmission=($admissionText -join "`n") | ConvertFrom-Json
    Require-Desktop ($fixedPairAdmission.status -ceq 'FIXED_NATIVE_PAIR_ADMITTED' -and $fixedPairAdmission.tooling.commit -ceq $env:GITHUB_SHA -and $fixedPairAdmission.tooling.tree -ceq $desktopTree) 'G39'
    $productCommit=$fixedPairAdmission.productBuild.sourceCommit
    $productTree=$fixedPairAdmission.productBuild.sourceTree
    foreach ($arch in @('x64','arm64')) {
      $pin=$fixedPairAdmission.inputs.$arch
      $prefix=$arch.ToUpperInvariant()
      foreach ($pair in @(@('ARTIFACT_ID',[string]$pin.id),@('ARTIFACT_DIGEST',$pin.digest),@('ARCHIVE_SHA',$pin.archive),@('RECEIPT_SHA',$pin.receipt),@('TREE',$productTree),@('SNAPSHOT',$fixedPairAdmission.productBuild.sourceSnapshotSha256))) {
        Require-Desktop ([Environment]::GetEnvironmentVariable($prefix+'_'+$pair[0]) -ceq $pair[1]) 'G40'
      }
    }
  } else {
    Require-Desktop ($env:REUSE_FIXED_NATIVE_PAIR -cne 'true') 'G41'
  }
  if ($desktopOperation -ceq 'Native') {
    # Intentional same-scope composition retains only the existing driver's
    # freshly created output paths. Its guards, build recipe and probes stay intact.
    Set-DesktopStage 'native-build'
    Require-Desktop ($env:PUBLISH_ENGINEERING_PREVIEW -ceq 'true') 'G10'
    . ./scripts/windows-engineering-qualification.ps1
    Require-Desktop ($qualificationMetadata.status -ceq 'PASSED' -and $targetTuple.architecture -ceq $env:QUALIFICATION_ARCHITECTURE) 'G11'
    Set-DesktopStage 'native-transfer'
    $transfer=Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-transfer-'+$env:QUALIFICATION_ARCHITECTURE+'-'+$env:GITHUB_RUN_ID)
    New-Item -ItemType Directory -Path $transfer | Out-Null
    $archiveHash=Hash-Desktop $candidateArchive
    Copy-Item -LiteralPath $candidateArchive -Destination (Join-Path $transfer 'native-layout.zip') -ErrorAction Stop
    Copy-Item -LiteralPath $evidencePath -Destination (Join-Path $transfer 'qualification.json') -ErrorAction Stop
    Require-Desktop ((Hash-Desktop (Join-Path $transfer 'native-layout.zip')) -ceq $archiveHash) 'G12'
    $receiptHash=Hash-Desktop $evidencePath
    Require-Desktop ((Hash-Desktop (Join-Path $transfer 'qualification.json')) -ceq $receiptHash) 'G13'
    Output-Desktop 'archive_sha' $archiveHash
    Output-Desktop 'receipt_sha' $receiptHash
    Output-Desktop 'source_tree' $desktopTree
    Output-Desktop 'snapshot_sha' $qualificationMetadata.sourceSnapshotSha256
    Write-Output ('DESKTOP_NATIVE_TRANSFER_READY architecture='+$targetTuple.architecture+' readerAccessible=true retentionDays=1')
  } else {
    Set-DesktopStage 'host-admission'
    . ./scripts/windows-engineering-preview.ps1
    $tuple=Get-PreviewTarget 'x64'
    Assert-PreviewNativeHost $tuple $env:PROCESSOR_ARCHITECTURE ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) ([Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString())
    Set-DesktopStage 'transfer-metadata'
    Require-Desktop ($env:X64_TREE -ceq $productTree -and $env:ARM64_TREE -ceq $productTree -and $env:X64_SNAPSHOT -ceq $env:ARM64_SNAPSHOT -and $env:X64_SNAPSHOT -cmatch '^[0-9a-f]{64}$') 'G14'
    foreach ($arch in @('X64','ARM64')) {
      Require-Desktop ([Environment]::GetEnvironmentVariable($arch+'_ARTIFACT_ID') -cmatch '^[0-9]{1,20}$') 'G15'
      foreach ($field in @('ARTIFACT_DIGEST','ARCHIVE_SHA','RECEIPT_SHA')) { Require-Desktop ([Environment]::GetEnvironmentVariable($arch+'_'+$field) -cmatch '^[0-9a-f]{64}$') 'G36' }
    }
    Require-Desktop ($env:X64_ARTIFACT_ID -cne $env:ARM64_ARTIFACT_ID) 'G16'
    if ($desktopOperation -ceq 'AdmitTransfers') {
      Write-Output 'DESKTOP_TRANSFER_IDS_ADMITTED exact immutable IDs only'
      return
    }
    Set-DesktopStage 'resource-path'
    Require-Desktop ($env:RUNNER_TEMP -cmatch '^[A-Za-z]:\\') 'G17'
    Set-DesktopStage 'disk-probe'
    $drive=Get-PSDrive -Name ([IO.Path]::GetPathRoot($env:RUNNER_TEMP).Substring(0,1))
    Set-DesktopStage 'memory-probe'
    $memory=Get-CimInstance -ClassName Win32_OperatingSystem
    Set-DesktopStage 'disk-admission'
    Require-Desktop ($drive.Free -ge 12GB) 'G18'
    Set-DesktopStage 'memory-admission'
    Require-Desktop (([long]$memory.TotalVisibleMemorySize * 1KB) -ge 8GB) 'G19'
    Set-DesktopStage 'private-parent'
    $parent=Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-combine-'+$env:GITHUB_RUN_ID)
    New-Item -ItemType Directory -Path $parent | Out-Null
    Set-DesktopStage 'python-selection'
    $python=(Get-Command python -CommandType Application -ErrorAction Stop | Select-Object -First 1).Path
    Set-DesktopStage 'python-admission'
    $pythonVersion=@(& $python -c 'import sys; assert sys.version_info[:2]>=(3,11); print("%d.%d.%d"%sys.version_info[:3])')
    $script:desktopExitCode=$LASTEXITCODE
    Require-Desktop ($script:desktopExitCode -eq 0 -and $pythonVersion.Count -eq 1 -and $pythonVersion[0] -cmatch '^3\.[0-9]{1,2}\.[0-9]{1,3}$') 'G20'
    foreach ($arch in @('x64','arm64')) {
      Set-DesktopStage ($arch+'-file-admission')
      $downloadInput=Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-download-'+$arch+'-'+$env:GITHUB_RUN_ID)
      $items=@(Get-ChildItem -LiteralPath $downloadInput -Force)
      Require-Desktop ($items.Count -eq 2 -and @($items | Where-Object { $_.PSIsContainer -or ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) }).Count -eq 0) 'G21'
      Require-Desktop (@($items.Name | Where-Object { $_ -cnotin @('native-layout.zip','qualification.json') }).Count -eq 0) 'G22'
      if ($ReuseFixedNativePair) {
        $receiptItem=Get-Item -LiteralPath (Join-Path $downloadInput 'qualification.json') -Force
        Require-Desktop ($receiptItem.Length -le 1MB) 'G42'
        $nativeReceipt=Get-Content -Raw -LiteralPath $receiptItem.FullName | ConvertFrom-Json
        Require-Desktop ($nativeReceipt.runId -is [string] -and $nativeReceipt.runId -ceq $fixedPairAdmission.productBuild.nativeRunId -and ($nativeReceipt.runAttempt -is [long] -or $nativeReceipt.runAttempt -is [int]) -and $nativeReceipt.runAttempt -eq 1) 'G43'
      }
      $toolArguments=@('-B','scripts/windows-desktop-transfer.py','--archive',(Join-Path $downloadInput 'native-layout.zip'),'--archive-sha',[Environment]::GetEnvironmentVariable($arch.ToUpperInvariant()+'_ARCHIVE_SHA'),'--receipt',(Join-Path $downloadInput 'qualification.json'),'--receipt-sha',[Environment]::GetEnvironmentVariable($arch.ToUpperInvariant()+'_RECEIPT_SHA'),'--destination',(Join-Path $parent $arch),'--architecture',$arch,'--source-commit',$productCommit,'--source-tree',$productTree)
      Set-DesktopStage ($arch+'-passive-replay')
      $result=@(& $python @toolArguments)
      $script:desktopExitCode=$LASTEXITCODE
      if ($script:desktopExitCode -ne 0) {
        $transferPhase='unknown';$transferCategory='unknown'
        if (($result -join '').Length -le 8192) {
          try {
            $transferFailure=($result -join "`n") | ConvertFrom-Json
            if ($transferFailure.status -is [string] -and $transferFailure.status -ceq 'REFUSED' -and $transferFailure.phase -is [string] -and $transferFailure.phase -cin @('paths','receipt-read','receipt-json','build-receipt','archive-read','zip-structure','preview-receipt','extraction','publication','passive-replay','unknown') -and $transferFailure.category -is [string] -and $transferFailure.category -cin @('guard','identity','freshness','io','format','marked','unknown')) {
              $transferPhase=$transferFailure.phase;$transferCategory=$transferFailure.category
            }
          } catch { }
        }
        $safeTransferFailure=[ordered]@{status='REFUSED';phase=$transferPhase;category=$transferCategory}
        Write-Output ('PASSIVE_TRANSFER_FAILURE '+($safeTransferFailure | ConvertTo-Json -Compress))
      }
      Require-Desktop ($script:desktopExitCode -eq 0 -and ($result -join '').Length -le 8192) 'G23'
      $proof=($result -join "`n") | ConvertFrom-Json
      Require-Desktop ($proof.status -ceq 'PASSIVE_TRANSFER_ONLY' -and $proof.architecture -ceq $arch -and $proof.sourceSnapshotSha256 -ceq $env:X64_SNAPSHOT) 'G24'
    }
    # One explicit closed portable preparation; never use the root SubStart
    # launcher, search PATH/ProgramFiles or install a compiler globally.
    $preparedDirectory=Join-Path $parent 'nsis-tool'
    Set-DesktopStage 'nsis-preparation'
    $preparedText=@(& $python -B scripts/prepare-nsis-tool.py --destination $preparedDirectory)
    $script:desktopExitCode=$LASTEXITCODE
    Require-Desktop ($script:desktopExitCode -eq 0 -and ($preparedText -join '').Length -le 16384) 'G25'
    Set-DesktopStage 'nsis-binding'
    $prepared=($preparedText -join "`n") | ConvertFrom-Json
    Require-Desktop ($prepared.version -is [string] -and $prepared.version -ceq '3.13' -and $prepared.compilerSha256 -is [string] -and $prepared.compilerSha256 -cmatch '^[0-9a-f]{64}$') 'G26'
    $nsisRoot=Join-Path $preparedDirectory 'nsis-3.13'
    $makensis=Join-Path $nsisRoot 'Bin/makensis.exe'
    Require-Desktop ($prepared.compiler -is [string] -and $prepared.compiler -ceq $makensis -and $prepared.resourceRoot -is [string] -and $prepared.resourceRoot -ceq $nsisRoot) 'G27'
    $nsisHash=Hash-Desktop $makensis
    Require-Desktop ($nsisHash -ceq $prepared.compilerSha256) 'G28'
    $resourceHashes=$prepared.resourceSha256
    # The pinned direct compiler has no RT_VERSION. Release version authority
    # is the validated archive receipt; PE metadata is optional evidence only.
    $versionInfo=[Diagnostics.FileVersionInfo]::GetVersionInfo($makensis)
    $nsisFileVersion=$null
    if ($versionInfo.FileVersion -is [string] -and $versionInfo.FileVersion -cmatch '^[0-9., ]{1,40}$') {
      $nsisFileVersion=$versionInfo.FileVersion
    }
    Set-DesktopStage 'product-version'
    $product=(Get-Content -Raw -LiteralPath Cargo.toml | Select-String -Pattern '(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+)"$').Matches
    Require-Desktop ($product.Count -eq 1) 'G29'
    $version=$product[0].Groups[1].Value
    $toolArguments=@('-B','scripts/package-windows-desktop.py','--source-commit',$productCommit,'--source-tree',$productTree,'--source-snapshot-sha256',$env:X64_SNAPSHOT,'--product-version',$version,'--makensis',$makensis,'--makensis-sha256',$nsisHash,'--output-directory',(Join-Path $parent 'installer'))
    foreach ($arch in @('x64','arm64')) { $toolArguments+=@(('--'+$arch+'-layout'),(Join-Path $parent ($arch+'/engineering-layout')),('--'+$arch+'-receipt'),(Join-Path $env:RUNNER_TEMP ('migration-stream-desktop-download-'+$arch+'-'+$env:GITHUB_RUN_ID+'/qualification.json')),('--'+$arch+'-receipt-sha256'),[Environment]::GetEnvironmentVariable($arch.ToUpperInvariant()+'_RECEIPT_SHA')) }
    Set-DesktopStage 'installer-compile'
    $result=@(& $python @toolArguments)
    $script:desktopExitCode=$LASTEXITCODE
    Require-Desktop ($script:desktopExitCode -eq 0 -and ($result -join '').Length -le 8192) 'G30'
    $proof=($result -join "`n") | ConvertFrom-Json
    Require-Desktop ($proof.ok -is [bool] -and $proof.ok -and $proof.installer.sha256 -cmatch '^[0-9a-f]{64}$') 'G31'
    Require-Desktop ((Hash-Desktop (Join-Path $parent 'installer/Migration Stream Setup.exe')) -ceq $proof.installer.sha256) 'G32'
    Set-DesktopStage 'postcompile-integrity'
    $replayedTool=@(& $python -B scripts/prepare-nsis-tool.py --validate-prepared $preparedDirectory)
    $script:desktopExitCode=$LASTEXITCODE
    Require-Desktop ($script:desktopExitCode -eq 0 -and ($replayedTool -join '').Length -le 16384 -and ($replayedTool -join '') -ceq ($preparedText -join '')) 'G33'
    $desktopStatus=@(& git status --porcelain=v1 --untracked-files=all)
    $script:desktopExitCode=$LASTEXITCODE
    Require-Desktop ($desktopStatus.Count -eq 0 -and $script:desktopExitCode -eq 0) 'G34'
    Set-DesktopStage 'final-evidence'
    $evidence=[ordered]@{schemaVersion='migration-stream/windows-desktop-installer-qualification/v1';status='COMPILED_PASSIVE_BYTES_ONLY';sourceCommit=$productCommit;sourceTree=$productTree;sourceSnapshotSha256=$env:X64_SNAPSHOT;runId=$env:GITHUB_RUN_ID;runAttempt=1;pythonVersion=$pythonVersion[0];pythonSha256=(Hash-Desktop $python);nsisArchiveSha256=$prepared.archiveSha256;nsisPreparationReceiptSha256=$prepared.preparedReceiptSha256;nsisExeSha256=$nsisHash;nsisVersion=$prepared.version;nsisVersionEvidence='PINNED_ARCHIVE_RECEIPT';nsisFileVersion=$nsisFileVersion;nsisResourceSha256=$resourceHashes;x64ArtifactId=$env:X64_ARTIFACT_ID;x64ArtifactDigest=$env:X64_ARTIFACT_DIGEST;arm64ArtifactId=$env:ARM64_ARTIFACT_ID;arm64ArtifactDigest=$env:ARM64_ARTIFACT_DIGEST;installerSha256=$proof.installer.sha256;installerBytes=$proof.installer.bytes;installerExecuted=$false;setupUploaded=$false;transferRetentionDays=1}
    if ($ReuseFixedNativePair) {
      $evidence['reusedInputs']=$true
      $evidence['productBuild']=$fixedPairAdmission.productBuild
      $evidence['tooling']=$fixedPairAdmission.tooling
    }
    $json=$evidence | ConvertTo-Json -Depth 6 -Compress
    Require-Desktop ($json.Length -le 8192) 'G35'
    Write-Output $json
  }
} catch {
  $failure=[ordered]@{status='REFUSED';stage=$script:desktopStage;guard=$script:desktopGuard;category=$script:desktopFailureCategory;exitCode=$script:desktopExitCode}
  Write-Output ('DESKTOP_QUALIFICATION_FAILURE '+($failure | ConvertTo-Json -Compress))
  Write-Output 'Desktop installer qualification refused; partials and explicitly unqualified transfer artifacts preserved'
  exit 1
}
