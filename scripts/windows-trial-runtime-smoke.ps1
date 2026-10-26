# First-run initialization/window + orderly close only, on a disposable Actions user.
param(
  [Parameter(Mandatory)][string] $ExpectedSourceRevision,
  [Parameter(Mandatory)][string] $ExpectedInstallerSha256,
  [Parameter(Mandatory)][string] $ExpectedExecutableSha256
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-install-checks.ps1"
. "$PSScriptRoot/windows-trial-staging-checks.ps1"
. "$PSScriptRoot/windows-trial-runtime-checks.ps1"
. "$PSScriptRoot/windows-trial-binary-identity.ps1"
$context = @{
  actions = "$env:GITHUB_ACTIONS"; repository = "$env:GITHUB_REPOSITORY"; event = "$env:GITHUB_EVENT_NAME"
  ref = "$env:GITHUB_REF"; runner = "$env:RUNNER_ENVIRONMENT"; os = "$env:RUNNER_OS"
  architecture = "$env:RUNNER_ARCH"; processArchitecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
  windows = $IsWindows; sha = "$env:GITHUB_SHA"; expectedSha = $ExpectedSourceRevision
  runId = "$env:GITHUB_RUN_ID"; runAttempt = "$env:GITHUB_RUN_ATTEMPT"; installerHash = $ExpectedInstallerSha256
}
Assert-TrialSmokeEnvironment $context
Assert-TrialRuntimeOverridesAbsent @(Get-ChildItem Env: | ForEach-Object Name)
if ($ExpectedExecutableSha256 -cnotmatch '^[a-f0-9]{64}$') { throw 'Exact built trial SHA-256 required.' }
$root = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))
if ($root -ine [IO.Path]::GetFullPath($env:GITHUB_WORKSPACE)) { throw 'Runtime smoke must use the exact workflow checkout.' }

function Assert-DirectRuntimePath([string] $Path) {
  $current = [IO.Path]::GetFullPath($Path)
  while ($current) {
    if ((Test-Path -LiteralPath $current) -and ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
      throw 'Runtime smoke refuses reparse points.'
    }
    $current = Split-Path $current -Parent
  }
}
function Assert-FreshRuntimePath([string] $Path) {
  Assert-DirectRuntimePath $Path
  if (Test-Path -LiteralPath $Path) { throw 'Existing application data/state found; runtime smoke requires fresh trial state.' }
}
function Get-RuntimeHash([string] $Path) {
  Assert-DirectRuntimePath $Path
  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Read-OwnedStartupObservation {
  # Both fixed roots were absent before this exact process was launched.
  Assert-DirectRuntimePath $trialData
  $log = Join-Path $trialData 'zephium.log'; Assert-DirectRuntimePath $log
  $crash = Join-Path $trialData 'last-crash.txt'; Assert-DirectRuntimePath $crash
  if (Test-Path -LiteralPath $crash) { return 'crash-report-present' }
  if (-not (Test-Path -LiteralPath $log)) { return 'waiting' }
  $stream = [IO.FileStream]::new($log, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
  try {
    if ($stream.Length -gt 2097152) { return 'log-size-limit' }
    $bytes = [byte[]]::new(2097152)
    $read = 0
    while ($read -lt $bytes.Length) {
      $count = $stream.Read($bytes, $read, $bytes.Length - $read)
      if ($count -eq 0) { break }; $read += $count
    }
    try { $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes, 0, $read) }
    catch [Text.DecoderFallbackException] { return 'log-encoding-invalid' }
    return Get-TrialStartupObservation -Log $text
  } finally { $stream.Dispose() }
}

$owner = $null
$result = [ordered]@{
  scope = 'fresh raw-compiler-executable initialization/window and orderly close only; NSIS payload identity is checked separately'
  sourceRevision = $ExpectedSourceRevision; executableSha256 = $ExpectedExecutableSha256
  workflowRunId = $context.runId; workflowRunAttempt = $context.runAttempt
  outcome = 'not-started'; observation = 'not-observed'; visibleResponsiveOwnedWindow = $false
  closeRequested = $false; closeMessagesPosted = 0; exitCode = $null; ownedJobEmpty = $false; forcedCleanup = $false; nativeLaunchError = 0
}
$outputPaths = Get-TrialOutputPaths
$resultDirectory = $outputPaths.RuntimeDirectory
$canWriteResult = $false
Push-Location $root
try {
  $revision = (git rev-parse HEAD).Trim()
  if ($LASTEXITCODE -ne 0) { throw 'Cannot read source revision.' }
  $status = git status --porcelain --untracked-files=normal | Out-String
  if ($LASTEXITCODE -ne 0) { throw 'Cannot establish clean source.' }
  Assert-TrialSourceProvenance -Revision $revision -SourceStatus $status -ExpectedSha $context.sha
  node scripts/windows-trial.mjs --check
  if ($LASTEXITCODE -ne 0) { throw 'Trial identity config guard failed.' }
  node --test scripts/windows-trial.test.mjs
  if ($LASTEXITCODE -ne 0) { throw 'Trial updater/default-browser/source guards failed.' }
  $artifact = $outputPaths.ArtifactDirectory; Assert-DirectRuntimePath $artifact
  $expectedFiles = @('BUILD-INFO.json', 'README.md', 'installer-hooks.nsh.txt', 'installer.nsi.txt', 'Zephium-Subscription-Trial-Windows-x64-setup.exe')
  $seen = @{}
  foreach ($line in @(Get-Content (Join-Path $artifact 'SHA256SUMS'))) {
    if ($line -cnotmatch '^([a-f0-9]{64})  ([A-Za-z0-9._-]+)$') { throw 'Invalid staged manifest.' }
    $hash = $Matches[1]; $name = $Matches[2]
    if ($expectedFiles -cnotcontains $name -or $seen.ContainsKey($name)) { throw 'Unexpected staged file.' }
    if ((Get-RuntimeHash (Join-Path $artifact $name)) -cne $hash) { throw 'Staged artifact hash mismatch.' }
    $seen[$name] = $true
  }
  if ($seen.Count -ne $expectedFiles.Count -or @(Get-ChildItem -LiteralPath $artifact -Force).Count -ne ($expectedFiles.Count + 1)) { throw 'Incomplete/unexpected staged artifact.' }
  if ((Get-RuntimeHash (Join-Path $artifact 'Zephium-Subscription-Trial-Windows-x64-setup.exe')) -cne $ExpectedInstallerSha256) { throw 'Unexpected staged installer.' }
  $info = Get-Content (Join-Path $artifact 'BUILD-INFO.json') -Raw | ConvertFrom-Json
  Assert-TrialSmokeBuildInfo $info $context
  Assert-TrialInstallerIdentity -Script (Get-Content (Join-Path $artifact 'installer.nsi.txt') -Raw)
  if ((Get-RuntimeHash (Join-Path $artifact 'installer-hooks.nsh.txt')) -cne (Get-RuntimeHash 'desktop/windows/subscription-trial-hooks.nsh')) { throw 'Staged hook differs from source.' }
  $executable = Join-Path $root 'target/release/zephium-subscription-trial.exe'
  if ((Get-RuntimeHash $executable) -cne $ExpectedExecutableSha256 -or (Get-Item -LiteralPath $executable).VersionInfo.ProductName -cne 'Zephium Subscription Trial') { throw 'Built executable identity mismatch.' }
  $binaryIdentity = Get-TrialBinaryIdentity -Path $executable
  Assert-TrialBinaryIdentityInfo -Info $info -Identity $binaryIdentity
  if ($ExpectedExecutableSha256 -cne $binaryIdentity.CompilerSha256) { throw 'Runtime smoke requires the raw compiler executable, not the NSIS-patched hash.' }
  if ((Get-AuthenticodeSignature -LiteralPath $executable).Status.ToString() -cnotin @('NotSigned', 'Valid')) { throw 'Executable signature status requires review.' }

  $profileRoot = [Environment]::GetFolderPath('UserProfile')
  $local = [Environment]::GetFolderPath('LocalApplicationData')
  $roaming = [Environment]::GetFolderPath('ApplicationData')
  foreach ($path in @($local, $roaming)) {
    if (-not $path -or -not ([IO.Path]::GetFullPath($path)).StartsWith(($profileRoot.TrimEnd('\') + '\'), [StringComparison]::OrdinalIgnoreCase)) { throw 'Runtime data root is outside disposable user profile.' }
    Assert-DirectRuntimePath $path
  }
  $trialData = Join-Path $roaming 'app.zephium.subscription-trial'
  $trialLocalData = Join-Path $local 'app.zephium.subscription-trial'
  foreach ($path in @($trialData, $trialLocalData, (Join-Path $local 'Zephium Subscription Trial'),
    (Join-Path $local 'Zephium'), (Join-Path $roaming 'app.zephium'), (Join-Path $local 'app.zephium'),
    (Join-Path $profileRoot '.zephium-native-v1/app.zephium.subscription-trial'), (Join-Path $profileRoot '.zephium-native-v1/app.zephium'))) {
    Assert-FreshRuntimePath $path
  }
  if (@(Get-Process -Name 'Zephium', 'zephium-desktop', 'zephium-subscription-trial', 'Zephium-Subscription-Trial-Windows-x64-setup' -ErrorAction SilentlyContinue).Count) { throw 'Existing app/installer process prevents runtime custody.' }
  foreach ($name in @('Zephium', 'Zephium Subscription Trial')) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Software\Microsoft\Windows\CurrentVersion\Uninstall\$name", $false)
    if ($null -ne $key) { $key.Dispose(); throw 'Installed app state prevents fresh runtime smoke.' }
  }
  Assert-FreshRuntimePath $resultDirectory
  New-Item -ItemType Directory -Path $resultDirectory | Out-Null
  $canWriteResult = $true
  Add-Type -Path "$PSScriptRoot/windows-trial-runtime-host.cs"
  $result.outcome = 'launch-failed'
  try { $owner = [ZephiumTrialRuntimeHost]::Start($ExpectedExecutableSha256) }
  catch {
    $failure = $_.Exception
    for ($depth = 0; $null -ne $failure -and $depth -lt 8; $depth++) {
      if ($failure -is [ZephiumTrialRuntimeHost+LaunchFailureException]) {
        $result.ownedJobEmpty = $failure.CleanupConfirmed
        $result.forcedCleanup = $failure.CreatedProcess
        $result.nativeLaunchError = $failure.NativeErrorCode
        if (-not $failure.CleanupConfirmed) { $result.outcome = 'cleanup-unconfirmed' }
        break
      }
      $failure = $failure.InnerException
    }
    throw 'Trial launch failed; the bounded runtime summary records whether owned-process cleanup was confirmed.'
  }
  $result.outcome = 'startup-not-ready'
  $timer = [Diagnostics.Stopwatch]::StartNew()
  while ($timer.Elapsed.TotalSeconds -lt 90) {
    $result.observation = Read-OwnedStartupObservation
    if ($result.observation -notin @('waiting', 'initialized-marker')) { throw "Startup blocked: $($result.observation). Raw log retained only in disposable profile." }
    if ($owner.HasExited) { $result.exitCode = $owner.ExitCode; throw 'Trial exited before observed initialization/window.' }
    $result.visibleResponsiveOwnedWindow = $owner.HasVisibleResponsiveWindow()
    if ($result.observation -eq 'initialized-marker' -and $result.visibleResponsiveOwnedWindow) { break }
    Start-Sleep -Milliseconds 200
  }
  if ($result.observation -ne 'initialized-marker' -or -not $result.visibleResponsiveOwnedWindow) { throw 'Startup observation timed out; process survival is not readiness.' }
  $result.outcome = 'close-failed'
  $result.closeRequested = $owner.RequestClose()
  $result.closeMessagesPosted = $owner.CloseMessagesPosted
  if (-not $result.closeRequested -or $result.closeMessagesPosted -ne 1) { throw 'The exact observed window did not accept one graceful close request.' }
  $result.ownedJobEmpty = $owner.WaitForEmpty(30000)
  if (-not $result.ownedJobEmpty) { throw 'Graceful close timed out or owned descendants remained.' }
  $result.exitCode = $owner.ExitCode
  $result.observation = Read-OwnedStartupObservation
  if ($result.exitCode -ne 0 -or $result.observation -ne 'initialized-marker') { throw 'Close returned nonzero or final startup diagnostics require review.' }
  $result.outcome = 'passed-initialization-window-and-close'
} finally {
  try {
    if ($null -ne $owner) {
      try {
        if (-not $owner.HasExited -or $owner.ActiveProcesses -ne 0) {
          # Exact job membership only; never PID/name/taskkill cleanup or profile deletion.
          $result.forcedCleanup = $true
          $result.ownedJobEmpty = $owner.StopOwned(10000)
          if (-not $result.ownedJobEmpty) { $result.outcome = 'cleanup-unconfirmed' }
        }
      } catch { $result.outcome = 'cleanup-unconfirmed'; $result.forcedCleanup = $true }
      finally { $owner.Dispose() }
    }
    if ($canWriteResult) {
      $result | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $resultDirectory 'summary.json') -Encoding utf8
      Write-Output "Runtime smoke: $($result.outcome); observation=$($result.observation); close_requested=$($result.closeRequested); close_messages=$($result.closeMessagesPosted); exit_code=$($result.exitCode); owned_job_empty=$($result.ownedJobEmpty); forced_cleanup=$($result.forcedCleanup)."
      Write-Output 'LIMIT: first-run initialization/window and orderly close only; no UI correctness, installed-app coexistence, accounts, navigation or inference tested. Full log is not uploaded.'
    }
  } finally { Pop-Location }
}
if ($result.outcome -ne 'passed-initialization-window-and-close' -or $result.forcedCleanup -or -not $result.ownedJobEmpty) {
  throw 'Runtime smoke did not establish both observed initialization/window and orderly complete exit.'
}
