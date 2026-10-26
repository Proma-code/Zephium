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
