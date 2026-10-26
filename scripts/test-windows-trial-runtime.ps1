$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-runtime-checks.ps1"
function Assert-Equal($Actual, $Expected) {
  if ($Actual -cne $Expected) { throw 'Runtime observation fixture mismatch.' }
}
function Assert-Rejected([scriptblock] $Check) {
  $failed = $false; try { & $Check } catch { $failed = $true }
  if (-not $failed) { throw 'Runtime safety fixture was accepted.' }
}
$ready = 'startup: main document initialized and window shown after 1234 ms'
Assert-Equal (Get-TrialStartupObservation '') 'waiting'
Assert-Equal (Get-TrialStartupObservation 'process started and is alive') 'waiting'
Assert-Equal (Get-TrialStartupObservation 'window shown') 'waiting'
Assert-Equal (Get-TrialStartupObservation $ready) 'initialized-marker'
Assert-Equal (Get-TrialStartupObservation ($ready + "`nsecurity: rejected runtime")) 'security-diagnostic-present'
Assert-Equal (Get-TrialStartupObservation ($ready + "`nstartup: failed to initialize Zephium: fixture")) 'startup-failure-marker'
Assert-Equal (Get-TrialStartupObservation ($ready + "`nruntime: terminal application shell failure: fixture")) 'startup-failure-marker'
Assert-Equal (Get-TrialStartupObservation ($ready + "`nstartup: native cleanup was not proven")) 'startup-failure-marker'
Assert-Equal (Get-TrialStartupObservation $ready $true) 'crash-report-present'
Assert-TrialRuntimeOverridesAbsent @('PATH', 'GITHUB_SHA')
foreach ($name in @('WEBVIEW2_USER_DATA_FOLDER', 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS', 'ZEPHIUM_NATIVE_STARTUP_PROBE', 'TAURI_CONFIG')) {
  Assert-Rejected { Assert-TrialRuntimeOverridesAbsent @('PATH', $name) }
}
$script = Get-Content "$PSScriptRoot/windows-trial-runtime-smoke.ps1" -Raw
foreach ($required in @('Assert-TrialSmokeEnvironment $context', 'Assert-TrialSourceProvenance', 'Assert-TrialSmokeBuildInfo',
  'Assert-TrialInstallerIdentity', 'Assert-FreshRuntimePath', '2097152', '$owner.HasVisibleResponsiveWindow()',
  '$owner.RequestClose()', '$owner.WaitForEmpty(30000)', '$owner.StopOwned(10000)', '$owner.Dispose()',
  "'initialized-marker'", 'summary.json')) {
  if (-not $script.Contains($required)) { throw "Missing runtime guard: $required" }
}
$executableScript = ($script -split "`n" | Where-Object { $_ -notmatch '^\s*#' }) -join "`n"
if ($executableScript -match '(?im)Remove-Item|DeleteSubKey|SetValue\(|Start-Process|Stop-Process|taskkill|Unblock-File|Set-MpPreference|Set-ExecutionPolicy|Invoke-WebRequest|SetEnvironmentVariable') {
  throw 'Runtime smoke includes an unauthorized mutation or unowned process operation.'
}
$hostSource = Get-Content "$PSScriptRoot/windows-trial-runtime-host.cs" -Raw
foreach ($required in @('FileShare.Read', 'SHA256.HashData(owner.executableLock)', 'JobListAttribute = new IntPtr(0x0002000D)',
  'ExtendedStartupInfo | CreateSuspended', 'IsProcessInJob(owner.process, owner.job', 'QueryFullProcessImageNameW',
  'TerminateJobObject(job, 1)', 'KillOnJobClose', 'PostMessageW(window, WmClose', 'WaitForSingleObject(process, 0)')) {
  if (-not $hostSource.Contains($required)) { throw "Missing native custody guard: $required" }
}
if ($hostSource -match 'OpenProcess\(|TerminateProcess\(|AssignProcessToJobObject\(|GetProcessById\(|SetEnvironmentVariable\(') { throw 'Unowned PID or non-atomic launch API introduced.' }
Add-Type -Path "$PSScriptRoot/windows-trial-runtime-host.cs"
foreach ($entry in @{ BasicLimits = 64; ExtendedLimits = 144; BasicAccounting = 48; StartupInfo = 104; StartupInfoEx = 112; ProcessInformation = 24 }.GetEnumerator()) {
  $type = [ZephiumTrialRuntimeHost].GetNestedType($entry.Key, [Reflection.BindingFlags]::NonPublic)
  $sizeMethod = [Runtime.InteropServices.Marshal].GetMethod('SizeOf', [type[]]@([type]))
  Assert-Equal ($sizeMethod.Invoke($null, @($type))) $entry.Value
}
if (-not $IsWindows) { Assert-Rejected { [ZephiumTrialRuntimeHost]::Start(('a' * 64)) } }
Write-Output 'Runtime observation/override/source guards and C# compilation/layout passed. No Windows app or native API was executed.'
