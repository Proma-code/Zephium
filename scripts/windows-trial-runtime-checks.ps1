# Pure classification only. No raw native log text is returned or printed.
function Get-TrialStartupObservation {
  param([AllowEmptyString()][string] $Log, [bool] $CrashReportPresent = $false)
  if ($CrashReportPresent) { return 'crash-report-present' }
  if ($Log -match 'security:') { return 'security-diagnostic-present' }
  if ($Log -match 'startup: failed to (initialize|build) Zephium|runtime: terminal|startup:.*(?:not proven|could not|cannot)') { return 'startup-failure-marker' }
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
