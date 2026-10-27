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
$shutdownCases = @{
  'shutdown: process exit did not complete within the hard deadline; forcing unsuccessful termination' = 'shutdown-hard-deadline'
  'privacy: privileged WebView2 Environment5/PID/HANDLE exit was not proven; leaving this run quarantined' = 'shutdown-webview-exit-unproven'
  'privacy: could not remove proven-exited privileged WebView2 runtime data at PRIVATE_PATH: PRIVATE_ERROR' = 'shutdown-runtime-cleanup-failed'
  'shutdown: Work operation cleanup was not proven' = 'shutdown-work-cleanup-unproven'
  'shutdown: final session durability was not proven before the deadline; exiting unsuccessfully' = 'shutdown-durability-unproven'
  'shutdown: shell actor did not acknowledge the barrier: PRIVATE_ERROR' = 'shutdown-barrier-unacknowledged'
  'shutdown: clean completion was not proven; exiting unsuccessfully' = 'shutdown-clean-completion-unproven'
  'shutdown: main-thread exit dispatch failed: PRIVATE_ERROR' = 'shutdown-exit-dispatch-failed'
  'shutdown: terminal signal could not arm the prepared hard-exit watchdog' = 'shutdown-watchdog-unavailable'
  'shutdown: hard-exit watchdog was not prepared; requesting immediate unsuccessful event-loop exit' = 'shutdown-watchdog-unavailable'
  'shutdown: exit requested before coordinator setup' = 'shutdown-before-setup'
  'shutdown: exit requested before shell setup' = 'shutdown-before-setup'
}
foreach ($entry in $shutdownCases.GetEnumerator()) {
  Assert-Equal (Get-TrialStartupObservation ($ready + "`n" + $entry.Key)) $entry.Value
  Assert-Equal (Get-TrialStartupObservation ($ready + "`n" + $entry.Key + "`nsecurity: fixed failure")) 'security-diagnostic-present'
  Assert-Equal (Get-TrialStartupObservation ($entry.Key + "`nstartup: failed to initialize Zephium: fixture")) 'startup-failure-marker'
}
Assert-Equal (Get-TrialStartupObservation ($ready + "`nshutdown: unknown note PRIVATE_VALUE")) 'initialized-marker'
Assert-Equal (Get-TrialStartupObservation ($ready + "`nprivacy: unrelated PRIVATE_VALUE")) 'initialized-marker'
Assert-Equal (Get-TrialStartupObservation ($ready + "`nshutdown: clean completion was not proven; fixture`nprivacy: could not remove proven-exited privileged WebView2 runtime data at PRIVATE_VALUE")) 'shutdown-runtime-cleanup-failed'
Assert-TrialRuntimeOverridesAbsent @('PATH', 'GITHUB_SHA')
foreach ($name in @('WEBVIEW2_USER_DATA_FOLDER', 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS', 'ZEPHIUM_NATIVE_STARTUP_PROBE', 'TAURI_CONFIG')) {
  Assert-Rejected { Assert-TrialRuntimeOverridesAbsent @('PATH', $name) }
}
$script = Get-Content "$PSScriptRoot/windows-trial-runtime-smoke.ps1" -Raw
foreach ($required in @('Assert-TrialSmokeEnvironment $context', 'Assert-TrialSourceProvenance', 'Assert-TrialSmokeBuildInfo',
  'Assert-TrialInstallerIdentity', 'Assert-FreshRuntimePath', '2097152', '$owner.HasVisibleResponsiveWindow()',
  '$owner.RequestClose()', '$owner.CloseMessagesPosted', '$result.closeMessagesPosted -ne 1',
  '$result.exitCode -ne 0', '$owner.WaitForEmpty(30000)', '$owner.StopOwned(10000)', '$owner.Dispose()',
  "'initialized-marker'", 'summary.json')) {
  if (-not $script.Contains($required)) { throw "Missing runtime guard: $required" }
}
$executableScript = ($script -split "`n" | Where-Object { $_ -notmatch '^\s*#' }) -join "`n"
if ($executableScript -match '(?im)Remove-Item|DeleteSubKey|SetValue\(|Start-Process|Stop-Process|taskkill|Unblock-File|Set-MpPreference|Set-ExecutionPolicy|Invoke-WebRequest|SetEnvironmentVariable') {
  throw 'Runtime smoke includes an unauthorized mutation or unowned process operation.'
}
if ($script.Contains('graceful=')) { throw 'A close request alone must not be reported as graceful shutdown.' }
$hostSource = Get-Content "$PSScriptRoot/windows-trial-runtime-host.cs" -Raw
foreach ($required in @('FileShare.Read', 'SHA256.HashData(owner.executableLock)', 'JobListAttribute = new IntPtr(0x0002000D)',
  'ExtendedStartupInfo | CreateSuspended', 'IsProcessInJob(owner.process, owner.job', 'QueryFullProcessImageNameW',
  'TerminateJobObject(job, 1)', 'KillOnJobClose', 'PostMessageW(observedWindow, WmClose', 'WaitForSingleObject(process, 0)',
  'if (observedWindow != IntPtr.Zero) return ObservedWindowStillReady()', 'if (closeAttempted) return false')) {
  if (-not $hostSource.Contains($required)) { throw "Missing native custody guard: $required" }
}
if ($hostSource -match 'OpenProcess\(|TerminateProcess\(|AssignProcessToJobObject\(|GetProcessById\(|SetEnvironmentVariable\(') { throw 'Unowned PID or non-atomic launch API introduced.' }
Add-Type -Path "$PSScriptRoot/windows-trial-runtime-host.cs"
$closeBody = ($hostSource -split 'public bool RequestClose\(\)', 2)[1] -split 'public bool WaitForEmpty', 2 | Select-Object -First 1
if ($closeBody -match 'foreach|OwnedWindows\(' -or ([regex]::Matches($closeBody, 'PostMessageW\(')).Count -ne 1) {
  throw 'Close must target exactly the retained observed window, never enumerate helpers.'
}
$identity = [ZephiumTrialRuntimeHost].GetMethod('SameWindowIdentity', [Reflection.BindingFlags]'NonPublic,Static')
foreach ($entry in @(
  @([IntPtr]123, [uint32]7, 'main-class', [IntPtr]123, [uint32]7, 'main-class', $true),
  @([IntPtr]0, [uint32]7, 'main-class', [IntPtr]0, [uint32]7, 'main-class', $false),
  @([IntPtr]123, [uint32]0, 'main-class', [IntPtr]123, [uint32]0, 'main-class', $false),
  @([IntPtr]123, [uint32]7, '', [IntPtr]123, [uint32]7, '', $false),
  @([IntPtr]123, [uint32]7, 'main-class', [IntPtr]124, [uint32]7, 'main-class', $false),
  @([IntPtr]123, [uint32]7, 'main-class', [IntPtr]123, [uint32]8, 'main-class', $false),
  @([IntPtr]123, [uint32]7, 'main-class', [IntPtr]123, [uint32]7, 'helper-class', $false)
)) { Assert-Equal ($identity.Invoke($null, [object[]]$entry[0..5])) $entry[6] }
$unlaunched = [Activator]::CreateInstance([ZephiumTrialRuntimeHost], $true)
Assert-Equal ($unlaunched.RequestClose()) $false
Assert-Equal ($unlaunched.RequestClose()) $false
Assert-Equal $unlaunched.CloseMessagesPosted 0
$unlaunched.Dispose()
foreach ($entry in @{ BasicLimits = 64; ExtendedLimits = 144; BasicAccounting = 48; StartupInfo = 104; StartupInfoEx = 112; ProcessInformation = 24 }.GetEnumerator()) {
  $type = [ZephiumTrialRuntimeHost].GetNestedType($entry.Key, [Reflection.BindingFlags]::NonPublic)
  $sizeMethod = [Runtime.InteropServices.Marshal].GetMethod('SizeOf', [type[]]@([type]))
  Assert-Equal ($sizeMethod.Invoke($null, @($type))) $entry.Value
}
if (-not $IsWindows) { Assert-Rejected { [ZephiumTrialRuntimeHost]::Start(('a' * 64)) } }
Write-Output 'Runtime observation/override/source guards and C# compilation/layout passed. No Windows app or native API was executed.'
