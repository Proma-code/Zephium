# Pure input checks; this file never installs, launches or changes OS state.
function Assert-TrialSmokeEnvironment {
  param([Parameter(Mandatory)][hashtable] $Context)
  foreach ($entry in @{
    actions = 'true'; repository = 'Proma-code/Zephium'; event = 'push'
    ref = 'refs/heads/windows/subscription-accounts'; runner = 'github-hosted'
    os = 'Windows'; architecture = 'X64'; processArchitecture = 'X64'
  }.GetEnumerator()) {
    if ($Context[$entry.Key] -cne $entry.Value) { throw "Installer smoke requires the authorized disposable runner: $($entry.Key)." }
  }
  if ($Context.windows -ne $true) { throw 'Installer smoke is Windows-only.' }
  if ($Context.sha -cnotmatch '^[a-f0-9]{40}$' -or $Context.expectedSha -cne $Context.sha) {
    throw 'Installer smoke source revision mismatch.'
  }
  if ($Context.runId -notmatch '^\d+$' -or $Context.runAttempt -notmatch '^\d+$') {
    throw 'Installer smoke requires exact workflow provenance.'
  }
  if ($Context.installerHash -cnotmatch '^[a-f0-9]{64}$') { throw 'An exact expected installer SHA-256 is required.' }
}

function Assert-TrialSmokeBuildInfo {
  param([Parameter(Mandatory)] $Info, [Parameter(Mandatory)][hashtable] $Context)
  foreach ($entry in @{
    sourceRevision = $Context.sha; sourceState = 'clean'
    workflowRunId = $Context.runId; workflowRunAttempt = $Context.runAttempt
    identifier = 'app.zephium.subscription-trial'; productName = 'Zephium Subscription Trial'
    executable = 'zephium-subscription-trial.exe'; installer = 'Zephium-Subscription-Trial-Windows-x64-setup.exe'
  }.GetEnumerator()) {
    if ($Info.($entry.Key) -cne $entry.Value) { throw "Staged installer provenance mismatch: $($entry.Key)." }
  }
}

# Keep installed payload bytes and product identity separate: neither substitutes
# for the other. Inputs are observations of the exact built/installed trial file.
function Assert-TrialInstalledBinary {
  param([string] $ActualHash, [string] $ExpectedHash, [AllowNull()][AllowEmptyString()][string] $ProductName)
  if ($ActualHash -cnotmatch '^[a-f0-9]{64}$' -or $ExpectedHash -cnotmatch '^[a-f0-9]{64}$') {
    throw 'Installed trial binary hash observation is invalid.'
  }
  if ($ActualHash -cne $ExpectedHash) { throw 'Installed trial executable bytes differ from the exact expected NSIS payload.' }
  if ($ProductName -cne 'Zephium Subscription Trial') { throw 'Installed trial executable product name is not isolated.' }
}

function Assert-TrialInstallerBinarySource {
  param([string] $Script, [string] $ExpectedPath)
  $matches = [regex]::Matches($Script, '(?m)^!define MAINBINARYSRCPATH "([^"\r\n]+)"\r?$')
  if ($matches.Count -ne 1 -or -not $ExpectedPath -or
      -not [string]::Equals($matches[0].Groups[1].Value, $ExpectedPath, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Generated installer payload source is not the exact built trial executable.'
  }
}
