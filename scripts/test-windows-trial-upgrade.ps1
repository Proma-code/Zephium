$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-upgrade-checks.ps1"
function Assert-Rejected([scriptblock] $Check) {
  $failed = $false; try { & $Check } catch { $failed = $true }
  if (-not $failed) { throw 'Unsafe upgrade fixture was accepted.' }
}
$pin = Get-DeliveredTrialPin
$candidate = @{ Name = 'provider-artifact.zip'; Directory = $false; Reparse = $false; Length = $pin.ZipBytes; Sha256 = $pin.ZipSha256 }
Assert-DeliveredArchiveCandidate @($candidate) $false
$fixed = $candidate.Clone(); $fixed.Name = $pin.ZipName
Assert-DeliveredArchiveCandidate @($fixed) $true
Assert-Rejected { Assert-DeliveredArchiveCandidate @($candidate) $true }
Assert-Rejected { Assert-DeliveredArchiveCandidate @() $false }
Assert-Rejected { Assert-DeliveredArchiveCandidate @($candidate, $candidate) $false }
foreach ($field in @('Directory', 'Reparse', 'Length', 'Sha256')) {
  $bad = $candidate.Clone()
  $bad[$field] = switch ($field) { 'Directory' { $true }; 'Reparse' { $true }; 'Length' { 1 }; 'Sha256' { '0' * 64 } }
  Assert-Rejected { Assert-DeliveredArchiveCandidate @($bad) $false }
}
$entries = @($pin.Files.Keys | ForEach-Object { @{ Name = $_; Length = $pin.Files[$_][1]; Sha256 = $pin.Files[$_][0]; Directory = $false; Reparse = $false } })
Assert-DeliveredArchiveEntries $entries
Assert-Rejected { Assert-DeliveredArchiveEntries $entries[0..4] }
Assert-Rejected { Assert-DeliveredArchiveEntries ($entries + $entries[0]) }
foreach ($patch in @(
  @{ Name = '../installer.exe' }, @{ Name = 'C:\installer.exe' }, @{ Name = 'nested/installer.exe' },
  @{ Directory = $true }, @{ Reparse = $true }, @{ Length = 0 }, @{ Sha256 = ('0' * 64) }
)) {
  $bad = @($entries | ForEach-Object { $_.Clone() })
  foreach ($key in $patch.Keys) { $bad[0][$key] = $patch[$key] }
  Assert-Rejected { Assert-DeliveredArchiveEntries $bad }
}
$duplicate = @($entries | ForEach-Object { $_.Clone() }); $duplicate[1] = $duplicate[0]
Assert-Rejected { Assert-DeliveredArchiveEntries $duplicate }
$info = @{
  sourceRevision = $pin.Source; sourceState = 'clean'; workflowRunId = $pin.Run; workflowRunAttempt = $pin.Attempt
  identifier = 'app.zephium.subscription-trial'; productName = 'Zephium Subscription Trial'; executable = 'zephium-subscription-trial.exe'
  installer = $pin.Installer; nsisExecutableSha256 = $pin.PayloadSha256; executableBytes = $pin.PayloadBytes
  compilerExecutableSha256 = '42bf17218dc593dee90253b274d59951489b2a287f50a8727891cbcbf530e48c'
  bundleIdentityMethod = 'tauri-cli-2.11.4-unsigned-nsis'; authenticodeStatus = 'NotSigned'
}
Assert-DeliveredTrialInfo $info
foreach ($field in $info.Keys) {
  $bad = $info.Clone(); $bad[$field] = 'other'
  Assert-Rejected { Assert-DeliveredTrialInfo $bad }
}
$replacement = @{ sourceRevision = ('a' * 40); nsisExecutableSha256 = ('b' * 64) }
Assert-DistinctTrialUpgrade $replacement ('c' * 64)
$sameSource = $replacement.Clone(); $sameSource.sourceRevision = $pin.Source
Assert-Rejected { Assert-DistinctTrialUpgrade $sameSource ('c' * 64) }
$samePayload = $replacement.Clone(); $samePayload.nsisExecutableSha256 = $pin.PayloadSha256
Assert-Rejected { Assert-DistinctTrialUpgrade $samePayload ('c' * 64) }
Assert-Rejected { Assert-DistinctTrialUpgrade $replacement $pin.InstallerSha256 }
$newline = $replacement.Clone(); $newline.sourceRevision += "`n"
Assert-Rejected { Assert-DistinctTrialUpgrade $newline ('c' * 64) }

Add-Type -Path "$PSScriptRoot/windows-trial-upgrade-credential.cs"
$suffix = '11111111111111111111111111111111'
if ([ZephiumTrialUpgradeCredential]::FixtureTarget($suffix) -cne "app.zephium.subscription-trial.upgrade-smoke.$suffix") { throw 'Credential fixture escaped the trial namespace.' }
foreach ($suffix in @('', 'app.zephium', '*', '../profile', '11111111-1111-1111-1111-111111111111', 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA')) {
  Assert-Rejected { [ZephiumTrialUpgradeCredential]::FixtureTarget($suffix) }
}
$type = [ZephiumTrialUpgradeCredential].GetNestedType('Credential', [Reflection.BindingFlags]::NonPublic)
$sizeMethod = [Runtime.InteropServices.Marshal].GetMethod('SizeOf', [type[]]@([type]))
if ($sizeMethod.Invoke($null, @($type)) -ne 80) { throw 'Unexpected x64 native credential fixture layout.' }
if (-not $IsWindows) { Assert-Rejected { [ZephiumTrialUpgradeCredential]::Create() } }
$helper = Get-Content "$PSScriptRoot/windows-trial-upgrade-checks.ps1" -Raw
$native = Get-Content "$PSScriptRoot/windows-trial-upgrade-credential.cs" -Raw
$smoke = Get-Content "$PSScriptRoot/windows-trial-install-smoke.ps1" -Raw
if ($helper -match 'Invoke-WebRequest|Invoke-RestMethod|Remove-Item|Expand-Archive|ExtractToDirectory' -or $native -match 'CredEnumerate|Process\.Start|File\.Delete') { throw 'Upgrade helpers gained broad network/extraction/credential operations.' }
foreach ($required in @('Assert-DistinctTrialUpgrade', 'Expand-DeliveredTrialBaseline', '$baseline.PayloadSha256', 'upgrade-smoke-settings.json', '$upgradeCredential.IsUnchanged()', '$upgradeCredential.RemoveOwned()')) {
  if (-not $smoke.Contains($required)) { throw "Missing delivered-upgrade guard: $required" }
}
Write-Output 'Delivered-build upgrade pins, archive negatives, distinct-build and trial credential guards passed; C# compiled. No installer or native credential API was executed.'
