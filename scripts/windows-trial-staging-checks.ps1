# Pure artifact checks shared by Windows staging and platform-neutral fixtures.
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
