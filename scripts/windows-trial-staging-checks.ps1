# Pure artifact checks shared by Windows staging and platform-neutral fixtures.
function ConvertTo-TrialWindowsAbsolutePath {
  param([string] $Path)
  # Only ordinary absolute drive paths, without Win32 normalization ambiguity.
  if ($Path -notmatch '^[A-Za-z]:\\[^\\]' -or $Path -match '[/\x00-\x1f<>"|?*]' -or $Path.Substring(2).Contains(':')) {
    throw 'Trial output parent must be an ordinary absolute Windows path.'
  }
  $normalized = $Path.TrimEnd('\')
  foreach ($part in $normalized.Substring(3).Split('\')) {
    if (-not $part -or $part -in @('.', '..') -or $part.EndsWith('.') -or $part.EndsWith(' ')) {
      throw 'Trial output parent contains an ambiguous path component.'
    }
  }
  return $normalized
}

# Pure resolver for fixtures. Production callers use only the no-argument wrapper.
function Resolve-TrialOutputPaths {
  param([Parameter(Mandatory)][hashtable] $Context)
  $source = ConvertTo-TrialWindowsAbsolutePath $Context.sourceRoot
  if ([string]::IsNullOrEmpty([string]$Context['actions'])) {
    foreach ($key in @('repository', 'runner', 'os', 'architecture', 'runId', 'runAttempt', 'workspace', 'temp', 'event', 'ref')) {
      if (-not [string]::IsNullOrEmpty([string]$Context[$key])) { throw 'Incomplete CI context cannot fall back to local trial output.' }
    }
    return [pscustomobject]@{
      ArtifactDirectory = "$source\target\windows-subscription-trial-artifact"
      RuntimeDirectory = "$source\target\windows-trial-runtime-smoke"
    }
  }
  foreach ($entry in @{
    actions = 'true'; repository = 'Proma-code/Zephium'; runner = 'github-hosted'
    os = 'Windows'; architecture = 'X64'; event = 'push'; ref = 'refs/heads/windows/subscription-accounts'
  }.GetEnumerator()) {
    if ($Context[$entry.Key] -cne $entry.Value) { throw "Trial output requires the authorized hosted workflow: $($entry.Key)." }
  }
  if ([string]$Context.runId -cnotmatch '\A[1-9][0-9]*\z' -or [string]$Context.runAttempt -cnotmatch '\A[1-9][0-9]*\z') {
    throw 'Trial output requires exact positive workflow run/attempt IDs.'
  }
  $workspace = ConvertTo-TrialWindowsAbsolutePath $Context.workspace
  $temporary = ConvertTo-TrialWindowsAbsolutePath $Context.temp
  if (-not [string]::Equals($source, $workspace, [StringComparison]::OrdinalIgnoreCase)) { throw 'Trial output checkout differs from workflow workspace.' }
  if ([string]::Equals($temporary, $workspace, [StringComparison]::OrdinalIgnoreCase) -or
      $temporary.StartsWith(($workspace + '\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Trial CI output must be outside the cached checkout/target tree.'
  }
  $parent = "$temporary\zephium-windows-trial-$($Context.runId)-$($Context.runAttempt)"
  if ($workspace.StartsWith(($parent + '\'), [StringComparison]::OrdinalIgnoreCase) -or
      [string]::Equals($parent, $workspace, [StringComparison]::OrdinalIgnoreCase)) { throw 'Trial output cannot contain the workflow checkout.' }
  return [pscustomobject]@{ ArtifactDirectory = "$parent\artifact"; RuntimeDirectory = "$parent\runtime" }
}

function Get-TrialOutputPaths {
  if (-not $IsWindows) { throw 'Trial output paths must be resolved on Windows.' }
  $context = @{
    sourceRoot = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))
    actions = "$env:GITHUB_ACTIONS"; repository = "$env:GITHUB_REPOSITORY"; runner = "$env:RUNNER_ENVIRONMENT"
    os = "$env:RUNNER_OS"; architecture = "$env:RUNNER_ARCH"; runId = "$env:GITHUB_RUN_ID"; runAttempt = "$env:GITHUB_RUN_ATTEMPT"
    workspace = "$env:GITHUB_WORKSPACE"; temp = "$env:RUNNER_TEMP"; event = "$env:GITHUB_EVENT_NAME"; ref = "$env:GITHUB_REF"
  }
  $paths = Resolve-TrialOutputPaths $context
  foreach ($path in @($context.sourceRoot, $paths.ArtifactDirectory, $paths.RuntimeDirectory)) {
    $current = [IO.Path]::GetFullPath($path)
    if (-not [string]::Equals($path, $current, [StringComparison]::OrdinalIgnoreCase)) { throw 'Trial output path was normalized unexpectedly.' }
    while ($current) {
      if ((Test-Path -LiteralPath $current) -and ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Trial output paths cannot use reparse points.'
      }
      $current = Split-Path $current -Parent
    }
  }
  if ($context.actions -and -not (Test-Path -LiteralPath $context.temp -PathType Container)) { throw 'Hosted runner temporary directory is missing.' }
  return $paths
}

function Assert-TrialSourceProvenance {
  param(
    [Parameter(Mandatory)][string] $Revision,
    [AllowEmptyString()][string] $SourceStatus = '',
    [AllowEmptyString()][string] $ExpectedSha = ''
  )
  if ($Revision -cnotmatch '^[a-f0-9]{40}$') { throw 'Expected an exact source commit SHA.' }
  if ($SourceStatus.Trim().Length -ne 0) {
    throw 'Refusing to stage a dirty source checkout. Commit/review source changes and rebuild first.'
  }
  if ($ExpectedSha -and $Revision -cne $ExpectedSha) {
    throw 'The source revision does not match the requested GitHub Actions commit.'
  }
}

function Assert-TrialInstallerIdentity {
  param([Parameter(Mandatory)][string] $Script)
  foreach ($line in @(
    '!define PRODUCTNAME "Zephium Subscription Trial"',
    '!define MAINBINARYNAME "zephium-subscription-trial"',
    '!define BUNDLEID "app.zephium.subscription-trial"',
    '!define INSTALLMODE "currentUser"',
    '!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"'
  )) {
    if (-not $Script.Contains($line)) { throw "Generated installer isolation mismatch: $line" }
  }
  if ($Script -notmatch 'subscription-trial-hooks\.nsh' -or $Script -match '(?m)^\s*!include\s+"[^"\r\n]*[\\/]installer-hooks\.nsh"') {
    throw 'Generated installer did not select only the isolated trial hooks.'
  }
  if ($Script -match '(?m)^\s*!insertmacro\s+(APP_ASSOCIATE|APP_ASSOCIATE_EX|APP_UNASSOCIATE)\b' -or $Script -match 'Software\\Classes\\(http|https|ZephiumURL)') {
    throw 'Unexpected browser/file association in trial installer.'
  }
}
