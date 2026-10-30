$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$qualificationParent = $null
$qualificationParentCreated = $false
# Only this bounded, fixed-key object and fixed stage labels reach logs.
$qualificationMetadata = [ordered]@{
  schemaVersion = 'migration-stream/windows-engineering-qualification/v1'
  status = 'FAILED'
  runId = $null
  runAttempt = 1
  stage = 'admission'
  commandExitCode = $null
  sourceCommit = $null
  sourceTree = $null
  architecture = $null
  target = $null
  runnerImageVersion = $null
  nodeVersion = '24.22.1'
  npmVersion = '12.2.1'
  rustVersion = '1.99.1'
  pythonVersion = $null
  toolSha256 = [ordered]@{}
  sourceSnapshotSha256 = $null
  frontendAssetsSha256 = $null
  documentLauncherSha256 = $null
  engineeringAssemblerSha256 = $null
  runtimeManifestSha256 = $null
  runtimeManifestFileSha256 = $null
  runtimeLauncherSha256 = $null
  runtimeControllerSha256 = $null
  integrityCliSha256 = $null
  workbenchSha256 = $null
  runtimeMemberCount = $null
  runtimeMemberBytes = $null
}
function Invoke-QualificationTool {
  param(
    [ValidateSet('rust-preparation','npm-preparation','npm-cache','cargo-cache','runtime-build','engineering-assembly')][string]$Stage,
    [string]$Program,
    [string[]]$Arguments
  )
  $script:qualificationMetadata.stage = $Stage
  $script:qualificationMetadata.commandExitCode = $null
  Write-Output ('QUALIFICATION stage=' + $Stage)
  $privateLog = Join-Path $qualificationParent ($Stage + '.private.log')
  $reservation = [IO.File]::Open($privateLog, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  $reservation.Dispose()
  & $Program @Arguments *>> $privateLog
  $script:qualificationMetadata.commandExitCode = $LASTEXITCODE
  # Post-return bound only; the finite job timeout is the external ceiling.
  if ($LASTEXITCODE -ne 0 -or (Get-Item -LiteralPath $privateLog).Length -gt 64MB) {
    throw 'qualification command refused or failed'
  }
}
function Get-QualificationTool([string]$Name) {
  $command = Get-Command $Name -CommandType Application -ErrorAction Stop | Select-Object -First 1
  $item = Get-Item -LiteralPath $command.Path -Force
  if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'qualification tool is not one concrete file'
  }
  return $item.FullName
}
function Get-QualificationHash([string]$Path) {
  $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
  if ($hash -cnotmatch '^[0-9a-f]{64}$') { throw 'qualification hash invalid' }
  return $hash
}
function Read-QualificationJson([string]$Path, [long]$MaximumBytes) {
  $item = Get-Item -LiteralPath $Path -Force
  if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.Length -gt $MaximumBytes) {
    throw 'qualification JSON file refused'
  }
  return Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json -ErrorAction Stop
}
function Assert-QualificationCount([object]$Value, [long]$Maximum) {
  if (($Value -isnot [int] -and $Value -isnot [long]) -or $Value -lt 1 -or $Value -gt $Maximum) {
    throw 'qualification integer count refused'
  }
}
function Assert-QualificationResultPath([object]$Value, [string]$Expected) {
  # Compare only two exact projections of our known drive path, without
  # following any caller/result-selected path or stripping namespaces.
  $full = [IO.Path]::GetFullPath($Expected)
  if ($full -cnotmatch '^[A-Za-z]:\\' -or $Value -isnot [string] -or $Value.Length -gt 4096) {
    throw 'qualification result path refused'
  }
  if (-not [string]::Equals($Value, $full, [StringComparison]::OrdinalIgnoreCase) -and
      -not [string]::Equals($Value, ('\\?\' + $full), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'qualification result path differs'
  }
}
try {
  $previewEnabled = switch -CaseSensitive ($env:PUBLISH_ENGINEERING_PREVIEW) {
    'false' { $false }
    'true' { $true }
    default { throw 'invalid explicit preview selection' }
  }
  if ($env:GITHUB_RUN_ATTEMPT -cne '1' -or $env:GITHUB_RUN_ID -cnotmatch '^[0-9]{1,20}$') { throw 'fresh manual run required' }
  $qualificationMetadata.runId = $env:GITHUB_RUN_ID
  if ($env:EXPECTED_SOURCE_COMMIT -cnotmatch '^[0-9a-f]{40}$' -or $env:GITHUB_SHA -cne $env:EXPECTED_SOURCE_COMMIT) {
    throw 'exact reviewed source commit required'
  }
  . (Join-Path (Get-Location).Path 'scripts/windows-engineering-preview.ps1')
  $targetTuple=Get-PreviewTarget $env:QUALIFICATION_ARCHITECTURE
  Assert-PreviewNativeHost $targetTuple $env:PROCESSOR_ARCHITECTURE ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) ([Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString())
  if ($env:RUSTUP_TOOLCHAIN -cne $targetTuple.toolchain) { throw 'exact native Rust selection required' }
  $qualificationMetadata.architecture=$targetTuple.architecture
  $qualificationMetadata.target=$targetTuple.target
  foreach ($name in @('RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','CARGO_TARGET_DIR','CARGO_BUILD_TARGET')) {
    if ([Environment]::GetEnvironmentVariable($name, 'Process')) { throw 'unplanned build override refused' }
  }
  $qualificationRepository = (Get-Location).Path
  $qualificationGit = Get-QualificationTool 'git.exe'
  $head = (@(& $qualificationGit rev-parse --verify HEAD 2>&1) -join '').Trim()
  if ($LASTEXITCODE -ne 0 -or $head -cne $env:EXPECTED_SOURCE_COMMIT) { throw 'checkout commit differs' }
  $tree = (@(& $qualificationGit rev-parse 'HEAD^{tree}' 2>&1) -join '').Trim()
  if ($LASTEXITCODE -ne 0 -or $tree -cnotmatch '^[0-9a-f]{40}$') { throw 'source tree invalid' }
  $sourceStatus = @(& $qualificationGit status --porcelain=v1 --untracked-files=all 2>&1)
  if ($LASTEXITCODE -ne 0 -or $sourceStatus.Count -ne 0) { throw 'clean source required' }
  $qualificationMetadata.sourceCommit = $head
  $qualificationMetadata.sourceTree = $tree
  $imageVersion = [Environment]::GetEnvironmentVariable('ImageVersion', 'Process')
  if ($imageVersion -cmatch '^[a-zA-Z0-9._-]{1,64}$') { $qualificationMetadata.runnerImageVersion = $imageVersion }
  if ($env:RUNNER_TEMP -cnotmatch '^[A-Za-z]:\\') { throw 'ordinary runner drive path required' }
  $drive = Get-PSDrive -Name ([IO.Path]::GetPathRoot($env:RUNNER_TEMP).Substring(0, 1))
  $memory = Get-CimInstance -ClassName Win32_OperatingSystem
  if ($drive.Free -lt 12GB -or ([long]$memory.TotalVisibleMemorySize * 1KB) -lt 8GB) {
    throw 'qualification resource preflight refused'
  }
  $qualificationParent = Join-Path $env:RUNNER_TEMP ('migration-stream-engineering-' + [Guid]::NewGuid())
  New-Item -ItemType Directory -Path $qualificationParent | Out-Null
  $qualificationParentCreated = $true
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
  $security = [Security.AccessControl.DirectorySecurity]::new()
  $security.SetOwner($sid)
  $security.SetAccessRuleProtection($true, $false)
  $rule = [Security.AccessControl.FileSystemAccessRule]::new(
    $sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
  $security.AddAccessRule($rule)
  [IO.FileSystemAclExtensions]::SetAccessControl([IO.DirectoryInfo]$qualificationParent, $security)
  $userConfig = Join-Path $qualificationParent 'user.npmrc'
  $globalConfig = Join-Path $qualificationParent 'global.npmrc'
  foreach ($config in @($userConfig, $globalConfig)) {
    $stream = [IO.File]::Open($config, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    $stream.Dispose()
  }
  $env:NPM_CONFIG_USERCONFIG = $userConfig
  $env:NPM_CONFIG_GLOBALCONFIG = $globalConfig
  $env:npm_config_cache = Join-Path $qualificationParent 'npm-cache'
  $env:npm_config_fetch_retries = '0'
  $env:npm_config_progress = 'false'
  $env:CARGO_HOME = Join-Path $qualificationParent 'cargo-home'
  $env:CARGO_NET_RETRY = '0'
  $env:CARGO_NET_OFFLINE = 'false'
  $env:npm_config_offline = 'false'
  $env:CARGO_BUILD_JOBS = '2'
  $qualificationNode = Get-QualificationTool 'node.exe'
  $qualificationBootstrapNpm = Get-QualificationTool 'npm.cmd'
  if ((@(& $qualificationNode -p "'v'+process.versions.node+' '+process.arch" 2>&1) -join '').Trim() -cne ('v24.22.1 '+$targetTuple.node) -or $LASTEXITCODE -ne 0 -or
      (Get-Content -Raw web/.node-version).Trim() -cne '24.22.1' -or
      (Read-QualificationJson 'web/package.json' 64KB).packageManager -cne 'npm@12.2.1') {
    throw 'pinned Node/npm source contract differs'
  }
  if (-not [string]::Equals((Split-Path -Parent $qualificationNode), (Split-Path -Parent $qualificationBootstrapNpm), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'bootstrap npm must accompany selected Node'
  }
  $qualificationPython = Get-QualificationTool 'python.exe'
  $pythonVersion = (@(& $qualificationPython --version 2>&1) -join '').Trim()
  if ($LASTEXITCODE -ne 0) { throw 'runner Python3 unavailable' }
  if ($pythonVersion -cmatch '^Python (3\.[0-9]{1,2}\.[0-9]{1,3})$') {
    $pythonNumber = [version]$Matches[1]
  } else { throw 'runner Python3 version refused' }
  if ($pythonNumber.Major -ne 3 -or $pythonNumber.Minor -lt 10) { throw 'runner Python3.10 or later required' }
  $qualificationMetadata.pythonVersion = $pythonNumber.ToString()
  $qualificationRustup = Get-QualificationTool 'rustup.exe'
  Invoke-QualificationTool 'rust-preparation' $qualificationRustup @('toolchain','install',$targetTuple.toolchain,'--profile','minimal')
  $qualificationCargo = (@(& $qualificationRustup which --toolchain $targetTuple.toolchain cargo 2>&1) -join '').Trim()
  if ($LASTEXITCODE -ne 0) { throw 'pinned Cargo unavailable' }
  $qualificationRustc = (@(& $qualificationRustup which --toolchain $targetTuple.toolchain rustc 2>&1) -join '').Trim()
  if ($LASTEXITCODE -ne 0) { throw 'pinned rustc unavailable' }
  $rustVersion = @(& $qualificationRustc -vV 2>&1)
  if ($LASTEXITCODE -ne 0) { throw 'pinned Rust unavailable' }
  Assert-PreviewRustHost $targetTuple $rustVersion
  $npmPrefix = Join-Path $qualificationParent 'npm-tool'
  Invoke-QualificationTool 'npm-preparation' $qualificationBootstrapNpm @('install','--prefix',$npmPrefix,'--ignore-scripts','--no-audit','--no-fund','npm@12.2.1')
  $qualificationNpm = Join-Path $npmPrefix 'node_modules/.bin/npm.cmd'
  if ((@(& $qualificationNpm --version 2>&1) -join '').Trim() -cne '12.2.1' -or $LASTEXITCODE -ne 0) { throw 'pinned npm unavailable' }
  $env:PATH = (Split-Path -Parent $qualificationCargo) + ';' + (Split-Path -Parent $qualificationNpm) + ';' + $env:PATH
  foreach ($entry in @(@('git',$qualificationGit),@('rustup',$qualificationRustup),@('node',$qualificationNode),@('python',$qualificationPython),@('cargo',$qualificationCargo),@('rustc',$qualificationRustc),@('npmShim',$qualificationNpm))) {
    $qualificationMetadata.toolSha256[$entry[0]] = Get-QualificationHash $entry[1]
  }
  # Online locked cache preparation is explicit. No product command yet.
  Invoke-QualificationTool 'npm-cache' $qualificationNpm @('--prefix','web','ci','--ignore-scripts','--no-audit','--no-fund')
  Invoke-QualificationTool 'cargo-cache' $qualificationCargo @('fetch','--locked','--target',$targetTuple.target)
  $env:CARGO_NET_OFFLINE = 'true'
  $env:npm_config_offline = 'true'
  $env:MIGRATION_STREAM_ENABLE_WINDOWS_NATIVE_ACCEPTANCE_CANDIDATE = '1'
  $qualificationRuntime = Join-Path $qualificationParent $targetTuple.artifact
  $qualificationRetained = Join-Path $qualificationParent 'migration-stream-private-bundle-build'
  $qualificationRuntimeResult = Join-Path $qualificationParent 'runtime-assembly.json'
  # This executes the existing mandatory metadata/Verify probes, including
  # product process/Tokio thread startup. It never selects Start/Open.
  $qualificationMetadata.stage = 'runtime-build'
  $qualificationMetadata.commandExitCode = $null
  Write-Output 'QUALIFICATION stage=runtime-build'
  $buildLog = Join-Path $qualificationParent 'runtime-build.private.log'
  $reservation = [IO.File]::Open($buildLog, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  $reservation.Dispose()
  & (Join-Path $qualificationRepository 'scripts/build-assisted-runtime-bundle.ps1') `
    -Output $qualificationRuntime -Target $targetTuple.target -NoArchive `
    -RetainedBuildRoot $qualificationRetained -AssemblerResultFile $qualificationRuntimeResult -RetainEngineeringTools *>> $buildLog
  $qualificationMetadata.commandExitCode = $LASTEXITCODE
  if ($LASTEXITCODE -ne 0 -or (Get-Item -LiteralPath $buildLog).Length -gt 64MB) { throw 'runtime build refused or failed' }
  $qualificationMetadata.stage = 'runtime-result'
  $runtimeResult = Read-QualificationJson $qualificationRuntimeResult 256KB
  if ($runtimeResult.ok -isnot [bool] -or $runtimeResult.schemaVersion -cne 'migration-stream/assisted-local-runtime/assembly/v1' -or $runtimeResult.ok -ne $true -or
      $runtimeResult.result.manifestSha256 -isnot [string] -or $runtimeResult.result.manifestSha256 -cnotmatch '^[0-9a-f]{64}$' -or
      $null -ne $runtimeResult.result.archivePath -or $null -ne $runtimeResult.result.archiveSha256 -or $null -ne $runtimeResult.result.archiveChecksumPath) {
    throw 'runtime assembly envelope refused'
  }
  Assert-QualificationCount $runtimeResult.result.memberCount 1024
  Assert-QualificationCount $runtimeResult.result.totalMemberBytes 512MB
  Assert-QualificationResultPath $runtimeResult.result.bundleDirectory $qualificationRuntime
  $qualificationMetadata.sourceSnapshotSha256 = Get-QualificationHash (Join-Path $qualificationRetained 'source-manifest.json')
  $qualificationMetadata.frontendAssetsSha256 = Get-QualificationHash (Join-Path $qualificationRetained 'frontend-manifest.json')
  $privateBinaries = Join-Path $qualificationRetained 'release-binaries'
  $releaseBinaries = Join-Path $qualificationRepository ('target/'+$targetTuple.target+'/release')
  foreach ($entry in @(@('documentLauncherSha256','migration-stream-document-open.exe'),@('engineeringAssemblerSha256','migration-stream-windows-engineering-layout.exe'))) {
    $copied = Get-QualificationHash (Join-Path $privateBinaries $entry[1])
    if ($copied -cne (Get-QualificationHash (Join-Path $releaseBinaries $entry[1]))) { throw 'external build copy differs' }
    $qualificationMetadata[$entry[0]] = $copied
  }
  $qualificationOutput = Join-Path $qualificationParent 'engineering-layout'
  Invoke-QualificationTool 'engineering-assembly' (Join-Path $privateBinaries 'migration-stream-windows-engineering-layout.exe') @(
    '--launcher',(Join-Path $privateBinaries 'migration-stream-document-open.exe'),
    '--expected-launcher-sha256',$qualificationMetadata.documentLauncherSha256,
    '--runtime',$qualificationRuntime,'--output-directory',$qualificationOutput,'--engineering-policy','local-unsigned-engineering')
  $qualificationMetadata.stage = 'static-verification'
  $engineering = Read-QualificationJson (Join-Path $qualificationParent 'engineering-assembly.private.log') 256KB
  if ($engineering.ok -isnot [bool] -or $engineering.ok -ne $true -or $engineering.result.documentLauncherSha256 -cne $qualificationMetadata.documentLauncherSha256) { throw 'engineering assembly envelope refused' }
  Assert-QualificationResultPath $engineering.result.outputDirectory $qualificationOutput
  $verified = $engineering.result.runtime
  Assert-QualificationCount $verified.memberCount 1024
  Assert-QualificationCount $verified.totalMemberBytes 512MB
  Assert-QualificationResultPath $verified.bundleRoot (Join-Path $qualificationOutput 'Runtime')
  if ($verified.productCommit -cne $head -or $verified.sourceState -cne 'CLEAN' -or
      $verified.sourceSnapshotSha256 -cne $qualificationMetadata.sourceSnapshotSha256 -or
      $verified.target.os -cne 'windows' -or $verified.target.architecture -cne $targetTuple.architecture -or
      $verified.target.targetTriple -cne $targetTuple.target -or
      $verified.manifestSha256 -cne $runtimeResult.result.manifestSha256 -or
      $verified.memberCount -ne $runtimeResult.result.memberCount -or
      $verified.totalMemberBytes -ne $runtimeResult.result.totalMemberBytes -or
      $verified.memberCount -lt 1 -or $verified.memberCount -gt 1024 -or
      $verified.totalMemberBytes -lt 1 -or $verified.totalMemberBytes -gt 512MB) { throw 'compiled runtime identity differs' }
  $sourceManifest = Join-Path $qualificationRuntime 'bundle-manifest.json'
  $copiedManifest = Join-Path $qualificationOutput 'Runtime/bundle-manifest.json'
  $qualificationMetadata.runtimeManifestFileSha256 = Get-QualificationHash $sourceManifest
  if ($qualificationMetadata.runtimeManifestFileSha256 -cne (Get-QualificationHash $copiedManifest) -or
      (Get-QualificationHash (Join-Path $qualificationOutput 'migration-stream-document-open.exe')) -cne $qualificationMetadata.documentLauncherSha256) {
    throw 'static copied bytes differ'
  }
  $outer = [IO.Directory]::EnumerateFileSystemEntries($qualificationOutput).GetEnumerator()
  $names = [Collections.Generic.List[string]]::new()
  try { while ($outer.MoveNext()) { if ($names.Count -ge 2) { throw 'unexpected outer material' }; $names.Add([IO.Path]::GetFileName($outer.Current)) } }
  finally { $outer.Dispose() }
  if ($names.Count -ne 2 -or -not $names.Contains('Runtime') -or -not $names.Contains('migration-stream-document-open.exe')) { throw 'outer layout differs' }
  foreach ($field in @('manifestSha256','launcherSha256','runtimeControllerSha256','integrityCliSha256','workbenchSha256')) {
    if ($verified.$field -isnot [string] -or $verified.$field -cnotmatch '^[0-9a-f]{64}$') { throw 'runtime digest invalid' }
  }
  if ($verified.launcherSha256 -cne $verified.runtimeControllerSha256) { throw 'runtime launcher differs from controller' }
  $qualificationMetadata.runtimeManifestSha256 = $verified.manifestSha256
  $qualificationMetadata.runtimeLauncherSha256 = $verified.launcherSha256
  $qualificationMetadata.runtimeControllerSha256 = $verified.runtimeControllerSha256
  $qualificationMetadata.integrityCliSha256 = $verified.integrityCliSha256
  $qualificationMetadata.workbenchSha256 = $verified.workbenchSha256
  $qualificationMetadata.runtimeMemberCount = $verified.memberCount
  $qualificationMetadata.runtimeMemberBytes = $verified.totalMemberBytes
  $sourceStatus = @(& $qualificationGit status --porcelain=v1 --untracked-files=all 2>&1)
  if ($LASTEXITCODE -ne 0 -or $sourceStatus.Count -ne 0) { throw 'source changed during qualification' }
  $qualificationMetadata.stage = 'complete'
  $qualificationMetadata.status = 'PASSED'
} catch {
  # Never expose exception objects, raw build output, paths or environment.
  $qualificationMetadata.status = 'FAILED'
}
$qualificationEvidence = $qualificationMetadata | ConvertTo-Json -Depth 5 -Compress
if ($qualificationEvidence.Length -gt 8192) { Write-Output 'QUALIFICATION evidence refused'; exit 1 }
if ($qualificationParentCreated -and (Test-Path -LiteralPath $qualificationParent)) {
  try {
    $evidencePath = Join-Path $qualificationParent 'qualification-evidence.json'
    $evidenceFile = [IO.File]::Open($evidencePath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
      $bytes = [Text.UTF8Encoding]::new($false).GetBytes($qualificationEvidence)
      $evidenceFile.Write($bytes, 0, $bytes.Length)
      $evidenceFile.Flush($true)
    } finally { $evidenceFile.Dispose() }
  } catch { Write-Output 'QUALIFICATION evidence refused'; exit 1 }
}
Write-Output $qualificationEvidence
if ($qualificationMetadata.status -ne 'PASSED') { exit 1 }
# Default false remains private/ephemeral. Never cast the string 'false' to [bool].
# Preview adds five fixed Verify calls: pack, local stage/final, download stage/final.
if ($previewEnabled -eq $true) {
  try {
    $candidateParent = Join-Path $env:RUNNER_TEMP ('migration-stream-engineering-candidate-' + $env:GITHUB_RUN_ID)
    New-Item -ItemType Directory -Path $candidateParent | Out-Null
    [IO.FileSystemAclExtensions]::SetAccessControl([IO.DirectoryInfo]$candidateParent, $security)
    $candidateName = ('migration-stream-engineering-candidate-'+$targetTuple.architecture+'-') + $head + '-' + $env:GITHUB_RUN_ID + '.zip'
    $candidateArchive = Join-Path $candidateParent $candidateName
    $previewHelper = Join-Path $qualificationRepository 'scripts/windows-engineering-preview.ps1'
    $candidateText = @(& $previewHelper -Operation Pack -Architecture $targetTuple.architecture -Layout $qualificationOutput -BuildReceipt $evidencePath -Archive $candidateArchive)
    if ($LASTEXITCODE -ne 0 -or ($candidateText -join '').Length -gt 8192) { throw 'candidate pack refused' }
    $candidate = ($candidateText -join "`n") | ConvertFrom-Json
    if ($candidate.status -cne 'CANDIDATE_LOCAL' -or $candidate.architecture -cne $targetTuple.architecture -or $candidate.target -cne $targetTuple.target -or $candidate.sourceCommit -cne $head -or $candidate.archiveSha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'candidate metadata refused' }
    $replayText = @(& $previewHelper -Operation Extract -Architecture $targetTuple.architecture -Archive $candidateArchive -ExpectedArchiveSha256 $candidate.archiveSha256 -Destination (Join-Path $qualificationParent 'candidate-local-replay'))
    if ($LASTEXITCODE -ne 0 -or ($replayText -join '').Length -gt 8192) { throw 'candidate local replay refused' }
    $replay = ($replayText -join "`n") | ConvertFrom-Json
    if ($replay.status -cne 'CANDIDATE_REPLAY_PASSED' -or $replay.architecture -cne $targetTuple.architecture -or $replay.target -cne $targetTuple.target -or $replay.archiveSha256 -cne $candidate.archiveSha256) { throw 'candidate replay differs' }
    [IO.File]::AppendAllText($env:GITHUB_OUTPUT, ('preview_sha256=' + $candidate.archiveSha256 + "`npreview_target="+$targetTuple.target+"`npreview_architecture="+$targetTuple.architecture+"`npreview_candidate_ready=true`n"))
    Write-Output 'Engineering candidate prepared; publication qualification pending download replay'
  } catch { Write-Output 'Engineering candidate preparation refused or failed'; exit 1 }
}
