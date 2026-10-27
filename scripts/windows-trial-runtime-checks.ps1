# Pure classification only. No raw native log text is returned or printed.
function Get-TrialStartupObservation {
  param([AllowEmptyString()][string] $Log, [bool] $CrashReportPresent = $false)
  if ($CrashReportPresent) { return 'crash-report-present' }
  if ($Log -match 'security:') { return 'security-diagnostic-present' }
  if ($Log -match 'startup: failed to (initialize|build) Zephium|runtime: terminal|startup:.*(?:not proven|could not|cannot)') { return 'startup-failure-marker' }
  # Known fixed source messages only; never return a suffix/path/provider value.
  if ($Log.Contains('shutdown: process exit did not complete within the hard deadline; forcing unsuccessful termination')) { return 'shutdown-hard-deadline' }
  if ($Log.Contains('privacy: privileged WebView2 Environment5/PID/HANDLE exit was not proven;')) { return 'shutdown-webview-exit-unproven' }
  if ($Log.Contains('privacy: could not remove proven-exited privileged WebView2 runtime data at ')) { return 'shutdown-runtime-cleanup-failed' }
  if ($Log.Contains('shutdown: Work operation cleanup was not proven')) { return 'shutdown-work-cleanup-unproven' }
  if ($Log.Contains('shutdown: final session durability was not proven before the deadline;')) { return 'shutdown-durability-unproven' }
  if ($Log.Contains('shutdown: shell actor did not acknowledge the barrier:')) { return 'shutdown-barrier-unacknowledged' }
  if ($Log.Contains('shutdown: clean completion was not proven;')) { return 'shutdown-clean-completion-unproven' }
  if ($Log.Contains('shutdown: main-thread exit dispatch failed:')) { return 'shutdown-exit-dispatch-failed' }
  if ($Log -match 'shutdown: (terminal signal could not arm the prepared hard-exit watchdog|hard-exit watchdog was not prepared;)') { return 'shutdown-watchdog-unavailable' }
  if ($Log -match 'shutdown: exit requested before (coordinator|shell) setup') { return 'shutdown-before-setup' }
  if ($Log -match 'startup: main document initialized and window shown after [0-9]+ ms') { return 'initialized-marker' }
  return 'waiting'
}

function Assert-TrialRuntimeOverridesAbsent {
  param([string[]] $Names)
  foreach ($name in $Names) {
    if ($name -match '^(WEBVIEW2_|ZEPHIUM_)|^TAURI_CONFIG$') {
      throw 'Runtime smoke refuses application/engine overrides; it does not disable admission gates.'
    }
  }
}
