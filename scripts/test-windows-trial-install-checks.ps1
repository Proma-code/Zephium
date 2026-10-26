$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-install-checks.ps1"

function Assert-Rejected([scriptblock] $Check) {
  $rejected = $false
  try { & $Check } catch { $rejected = $true }
  if (-not $rejected) { throw 'Unsafe installer-smoke fixture was accepted.' }
}
$context = @{
  actions = 'true'; repository = 'Proma-code/Zephium'; event = 'push'
  ref = 'refs/heads/windows/subscription-accounts'; runner = 'github-hosted'
  os = 'Windows'; architecture = 'X64'; processArchitecture = 'X64'; windows = $true
  sha = '0123456789abcdef0123456789abcdef01234567'
  expectedSha = '0123456789abcdef0123456789abcdef01234567'
  runId = '1234'; runAttempt = '1'; installerHash = ('a' * 64)
}
Assert-TrialSmokeEnvironment $context
foreach ($key in $context.Keys) {
  $bad = $context.Clone()
  $bad[$key] = if ($key -eq 'windows') { $false } else { 'untrusted' }
  Assert-Rejected { Assert-TrialSmokeEnvironment $bad }
}
$info = @{
  sourceRevision = $context.sha; sourceState = 'clean'; workflowRunId = '1234'; workflowRunAttempt = '1'
  identifier = 'app.zephium.subscription-trial'; productName = 'Zephium Subscription Trial'
  executable = 'zephium-subscription-trial.exe'; installer = 'Zephium-Subscription-Trial-Windows-x64-setup.exe'
}
Assert-TrialSmokeBuildInfo $info $context
foreach ($key in $info.Keys) {
  $bad = $info.Clone(); $bad[$key] = 'other'
  Assert-Rejected { Assert-TrialSmokeBuildInfo $bad $context }
}

$script = Get-Content "$PSScriptRoot/windows-trial-install-smoke.ps1" -Raw
foreach ($required in @(
  'Assert-TrialSmokeEnvironment $context', 'Assert-TrialSmokeBuildInfo $info $context',
  'Assert-TrialSourceProvenance', 'Assert-TrialInstallerIdentity',
  'Invoke-InstallerProcess $installer ''/S''', 'Invoke-InstallerProcess $installer ''/S /UPDATE''',
  'Assert-ProductionPreserved', 'Assert-NoAppProcess', 'Remove-OwnedFixtures',
  '$script:cleanupUnsafe = $true', '$process.WaitForExit(10000)',
  'if ($script:cleanupUnsafe)', 'if (-not $script:cleanupUnsafe -and -not $uninstalled'
)) {
  if (-not $script.Contains($required)) { throw "Missing smoke guard: $required" }
}
if ($script -match '(?im)Remove-Item[^\r\n]*-Recurse|DeleteSubKeyTree|Set-ExecutionPolicy|Unblock-File|Set-MpPreference|Start-Process|/R(?:\s|[\x27\x22])') {
  throw 'Smoke script contains a prohibited broad cleanup, launch or security bypass operation.'
}
Write-Output 'Installer smoke pure guards passed. No Windows installation or registry mutation was performed.'
