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

Write-Output 'Trial staging fixtures passed: clean provenance and isolated generated installer accepted; dirty/mismatched source and production identity/association rejected. No Windows installation was executed.'
