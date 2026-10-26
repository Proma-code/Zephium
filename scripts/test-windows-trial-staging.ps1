$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-staging-checks.ps1"

function Assert-Rejected {
  param([Parameter(Mandatory)][scriptblock] $Check)
  $rejected = $false
  try { & $Check } catch { $rejected = $true }
  if (-not $rejected) { throw 'An invalid artifact fixture was accepted.' }
}

$revision = '0123456789abcdef0123456789abcdef01234567'
Assert-TrialSourceProvenance -Revision $revision
Assert-TrialSourceProvenance -Revision $revision -ExpectedSha $revision
Assert-Rejected { Assert-TrialSourceProvenance -Revision 'unknown' }
Assert-Rejected { Assert-TrialSourceProvenance -Revision $revision -SourceStatus ' M desktop/src/lib.rs' }
Assert-Rejected { Assert-TrialSourceProvenance -Revision $revision -SourceStatus '?? unreviewed.rs' }
Assert-Rejected { Assert-TrialSourceProvenance -Revision $revision -ExpectedSha ('f' * 40) }

$installer = @'
!define PRODUCTNAME "Zephium Subscription Trial"
!define MAINBINARYNAME "zephium-subscription-trial"
!define BUNDLEID "app.zephium.subscription-trial"
!define INSTALLMODE "currentUser"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"
!include "C:\synthetic-fixture\subscription-trial-hooks.nsh"
'@
Assert-TrialInstallerIdentity -Script $installer
Assert-Rejected { Assert-TrialInstallerIdentity -Script ($installer.Replace('"Zephium Subscription Trial"', '"Zephium"')) }
Assert-Rejected { Assert-TrialInstallerIdentity -Script ($installer.Replace('"zephium-subscription-trial"', '"zephium-desktop"')) }
Assert-Rejected { Assert-TrialInstallerIdentity -Script ($installer.Replace('"app.zephium.subscription-trial"', '"app.zephium"')) }
Assert-Rejected { Assert-TrialInstallerIdentity -Script ($installer.Replace('"currentUser"', '"perMachine"')) }
Assert-Rejected { Assert-TrialInstallerIdentity -Script ($installer.Replace('subscription-trial-hooks.nsh', 'installer-hooks.nsh')) }
Assert-Rejected { Assert-TrialInstallerIdentity -Script ($installer + "`n" + '!insertmacro APP_ASSOCIATE "html" "Zephium"') }
Assert-Rejected { Assert-TrialInstallerIdentity -Script ($installer + "`n" + 'WriteRegStr HKCU "Software\Classes\ZephiumURL" "" ""') }

$local = Resolve-TrialOutputPaths @{ sourceRoot = 'C:\source\Zephium' }
if ($local.ArtifactDirectory -cne 'C:\source\Zephium\target\windows-subscription-trial-artifact') { throw 'Local staging default changed.' }
$context = @{
  sourceRoot = 'D:\a\Zephium\Zephium'; workspace = 'D:\a\Zephium\Zephium'; temp = 'D:\a\_temp'
  actions = 'true'; repository = 'Proma-code/Zephium'; runner = 'github-hosted'; os = 'Windows'; architecture = 'X64'
  event = 'push'; ref = 'refs/heads/windows/subscription-accounts'; runId = '12345'; runAttempt = '2'
}
$paths = Resolve-TrialOutputPaths $context
if ($paths.ArtifactDirectory -cne 'D:\a\_temp\zephium-windows-trial-12345-2\artifact' -or
    $paths.RuntimeDirectory -cne 'D:\a\_temp\zephium-windows-trial-12345-2\runtime') { throw 'CI output paths are not run/attempt scoped.' }
$retry = $context.Clone(); $retry.runAttempt = '3'
if ((Resolve-TrialOutputPaths $retry).ArtifactDirectory -ceq $paths.ArtifactDirectory) { throw 'Retry reused stale artifact output.' }
$otherRun = $context.Clone(); $otherRun.runId = '12346'
if ((Resolve-TrialOutputPaths $otherRun).RuntimeDirectory -ceq $paths.RuntimeDirectory) { throw 'Another run reused runtime observations.' }
foreach ($key in @('actions', 'repository', 'runner', 'os', 'architecture', 'event', 'ref', 'runId', 'runAttempt', 'workspace')) {
  $bad = $context.Clone(); $bad[$key] = 'invalid'
  Assert-Rejected { Resolve-TrialOutputPaths $bad }
}
foreach ($key in @('runId', 'runAttempt')) {
  foreach ($id in @("123`n", "123`r`n", "123`r", '0', '-1')) {
    $bad = $context.Clone(); $bad[$key] = $id
    Assert-Rejected { Resolve-TrialOutputPaths $bad }
  }
}
foreach ($temp in @('relative', 'C:relative', '\\server\share', 'D:\a\..\temp', 'D:\a\.\temp', 'D:\a\bad.\temp',
  'D:\a\bad \temp', 'D:\a\Zephium\Zephium', 'D:\a\Zephium\Zephium\target', 'D:\a\Zephium\Zephium\nested',
  'D:\a\_temp:stream', "D:\a\_temp`nother")) {
  $bad = $context.Clone(); $bad.temp = $temp
  Assert-Rejected { Resolve-TrialOutputPaths $bad }
}
Assert-Rejected { Resolve-TrialOutputPaths @{ sourceRoot = 'C:\source\Zephium'; runId = '12345' } }
Assert-Rejected { Resolve-TrialOutputPaths @{ sourceRoot = 'relative' } }
foreach ($file in @('stage-windows-trial.ps1', 'windows-trial-install-smoke.ps1', 'windows-trial-runtime-smoke.ps1')) {
  $source = Get-Content "$PSScriptRoot/$file" -Raw
  if (-not $source.Contains('Get-TrialOutputPaths') -or $source.Contains("'target/windows-subscription-trial-artifact'") -or $source.Contains("'target/windows-trial-runtime-smoke'")) {
    throw 'A staging/smoke consumer bypasses fresh output resolution.'
  }
}
Write-Output 'Trial staging fixtures passed: source/installer isolation and fresh hosted run/attempt output paths; unsafe paths/contexts rejected. No Windows installation was executed.'
