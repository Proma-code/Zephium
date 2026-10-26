# Disposable GitHub-hosted runner only. Synthetic preservation, not real-app coexistence.
param(
  [Parameter(Mandatory)][string] $ExpectedSourceRevision,
  [Parameter(Mandatory)][string] $ExpectedInstallerSha256,
  [switch] $UpgradeFromDeliveredTrial
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-install-checks.ps1"
. "$PSScriptRoot/windows-trial-staging-checks.ps1"
. "$PSScriptRoot/windows-trial-binary-identity.ps1"
. "$PSScriptRoot/windows-trial-upgrade-checks.ps1"
$context = @{
  actions = "$env:GITHUB_ACTIONS"; repository = "$env:GITHUB_REPOSITORY"; event = "$env:GITHUB_EVENT_NAME"
  ref = "$env:GITHUB_REF"; runner = "$env:RUNNER_ENVIRONMENT"; os = "$env:RUNNER_OS"
  architecture = "$env:RUNNER_ARCH"; processArchitecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
  windows = $IsWindows; sha = "$env:GITHUB_SHA"; expectedSha = $ExpectedSourceRevision
  runId = "$env:GITHUB_RUN_ID"; runAttempt = "$env:GITHUB_RUN_ATTEMPT"; installerHash = $ExpectedInstallerSha256
}
Assert-TrialSmokeEnvironment $context
$script:cleanupUnsafe = $false
$root = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))
if ($root -ine [IO.Path]::GetFullPath($env:GITHUB_WORKSPACE)) { throw 'Smoke must use the exact workflow checkout.' }

function Assert-NoReparse([string] $Path) {
  $itemPath = [IO.Path]::GetFullPath($Path)
  while ($itemPath) {
    if (Test-Path -LiteralPath $itemPath) {
      if ((Get-Item -LiteralPath $itemPath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Refusing a reparse point in an installer or fixture path.'
      }
    }
    $itemPath = Split-Path $itemPath -Parent
  }
}
function Assert-Absent([string] $Path) {
  Assert-NoReparse $Path
  if (Test-Path -LiteralPath $Path) { throw 'Existing application/fixture state found; refusing to overwrite it.' }
}
function Get-Hash([string] $Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function Assert-NoAppProcess {
  if (@(Get-Process -Name 'Zephium', 'zephium-desktop', 'zephium-subscription-trial' -ErrorAction SilentlyContinue).Count) {
    $script:cleanupUnsafe = $true
    throw 'An application process exists. This smoke never launches or terminates the application.'
  }
}
function Read-RegistryTree([Microsoft.Win32.RegistryKey] $Base, [string] $Path) {
  $key = $Base.OpenSubKey($Path, $false)
  if ($null -eq $key) { return 'absent' }
  try {
    $values = [ordered]@{}
    foreach ($name in @($key.GetValueNames() | Sort-Object)) {
      $values[$name] = @($key.GetValueKind($name).ToString(), $key.GetValue($name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames))
    }
    $children = [ordered]@{}
    foreach ($name in @($key.GetSubKeyNames() | Sort-Object)) { $children[$name] = Read-RegistryTree $key $name }
    return ([ordered]@{ values = $values; children = $children } | ConvertTo-Json -Depth 30 -Compress)
  } finally { $key.Dispose() }
}
function Read-RegistryValue([string] $Path, [string] $Name) {
  $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($Path, $false)
  if ($null -eq $key) { return 'absent' }
  try {
    if ($key.GetValueNames() -notcontains $Name) { return 'absent' }
    return (@($key.GetValueKind($Name).ToString(), $key.GetValue($Name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)) | ConvertTo-Json -Compress)
  } finally { $key.Dispose() }
}
function Invoke-InstallerProcess([string] $File, [string] $Arguments, [string] $ExpectedHash) {
  if ($script:cleanupUnsafe) { throw 'Installer process custody is uncertain; no further native operation is allowed.' }
  Assert-NoAppProcess
  Assert-NoReparse $File
  if ($ExpectedHash -cnotmatch '^[a-f0-9]{64}$' -or (Get-Hash $File) -cne $ExpectedHash) { throw 'Executable changed before installer invocation.' }
  $start = [Diagnostics.ProcessStartInfo]::new()
  $start.FileName = $File; $start.Arguments = $Arguments; $start.UseShellExecute = $false
  $process = [Diagnostics.Process]::Start($start)
  try {
    if (-not $process.WaitForExit(120000)) {
      # Only this exact child tree, never an application-name process search.
      # Root exit cannot prove every descendant stopped: any timeout permanently
      # disables further uninstall/fixture cleanup on this disposable runner.
      $script:cleanupUnsafe = $true
      $process.Kill($true)
      if (-not $process.WaitForExit(10000)) { throw 'Timed-out installer did not confirm exit; state is left for runner teardown.' }
      throw 'Installer timed out; root stopped, but descendant custody is unproven. State is left for runner teardown.'
    }
    if ($process.ExitCode -ne 0) { throw "Installer exited unsuccessfully ($($process.ExitCode))." }
  } catch {
    try { if (-not $process.HasExited) { $script:cleanupUnsafe = $true } }
    catch { $script:cleanupUnsafe = $true }
    throw
  } finally { $process.Dispose() }
  Assert-NoAppProcess
}

$fixtureFiles = [Collections.Generic.List[object]]::new()
$fixtureDirectories = [Collections.Generic.List[string]]::new()
$fixtureValues = [Collections.Generic.List[object]]::new()
$fixtureKeys = [Collections.Generic.List[string]]::new()
$fixtureId = [Guid]::NewGuid().ToString('N')
function New-FixtureDirectory([string] $Path) {
  Assert-Absent $Path
  New-Item -ItemType Directory -Path $Path | Out-Null
  $fixtureDirectories.Add($Path)
}
function Save-FixtureFile([string] $Path, [string] $Text) {
  Assert-Absent $Path
  [IO.File]::WriteAllText($Path, $Text)
  $fixtureFiles.Add(@{ path = $Path; hash = (Get-Hash $Path) })
}
function Save-FixtureValue([string] $Path, [string] $Name, $Data, [Microsoft.Win32.RegistryValueKind] $Kind) {
  if ((Read-RegistryValue $Path $Name) -cne 'absent') { throw 'Refusing to replace an existing registry value.' }
  $old = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($Path, $false)
  if ($null -eq $old) { $fixtureKeys.Add($Path) } else { $old.Dispose() }
  $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($Path)
  try { $key.SetValue($Name, $Data, $Kind) } finally { $key.Dispose() }
  $fixtureValues.Add(@{ path = $Path; name = $Name; snapshot = (Read-RegistryValue $Path $Name) })
}
function Remove-OwnedFixtures {
  $incomplete = $false
  foreach ($entry in $fixtureValues) {
    $current = Read-RegistryValue $entry.path $entry.name
    if ($current -ceq 'absent') { continue }
    if ($current -cne $entry.snapshot) { $incomplete = $true; Write-Warning 'Changed fixture registry value left untouched for runner disposal.'; continue }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($entry.path, $true)
    try { $key.DeleteValue($entry.name, $false) } finally { $key.Dispose() }
  }
  foreach ($path in $fixtureKeys) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($path, $false)
    if ($null -ne $key) {
      $empty = $key.ValueCount -eq 0 -and $key.SubKeyCount -eq 0; $key.Dispose()
      if ($empty) { [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKey($path, $false) }
    }
  }
  foreach ($entry in $fixtureFiles) {
    Assert-NoReparse $entry.path
    if (Test-Path -LiteralPath $entry.path) {
      if ((Get-Hash $entry.path) -ceq $entry.hash) { Remove-Item -LiteralPath $entry.path }
      else { $incomplete = $true; Write-Warning 'Changed fixture file left untouched for runner disposal.' }
    }
  }
  for ($i = $fixtureDirectories.Count - 1; $i -ge 0; $i--) {
    $path = $fixtureDirectories[$i]; Assert-NoReparse $path
    if ((Test-Path -LiteralPath $path) -and @(Get-ChildItem -LiteralPath $path -Force).Count -eq 0) { Remove-Item -LiteralPath $path }
  }
  if ($incomplete) { throw 'Fixture cleanup refused changed state; disposable-runner teardown is required.' }
}

Push-Location $root
$uninstaller = $null; $uninstallerHash = $null; $uninstalled = $false; $upgradeCredential = $null; $baseline = $null
try {
  $revision = (git rev-parse HEAD).Trim()
  if ($LASTEXITCODE -ne 0) { throw 'Cannot read workflow source revision.' }
  $status = git status --porcelain --untracked-files=normal | Out-String
  if ($LASTEXITCODE -ne 0) { throw 'Cannot verify clean workflow checkout.' }
  Assert-TrialSourceProvenance -Revision $revision -SourceStatus $status -ExpectedSha $context.sha
  node scripts/windows-trial.mjs --check
  if ($LASTEXITCODE -ne 0) { throw 'Trial build configuration guard failed.' }
  node --test scripts/windows-trial.test.mjs
  if ($LASTEXITCODE -ne 0) { throw 'Trial installer/updater source contract failed.' }

  $artifact = (Get-TrialOutputPaths).ArtifactDirectory
  Assert-NoReparse $artifact
  $expectedFiles = @('BUILD-INFO.json', 'README.md', 'installer-hooks.nsh.txt', 'installer.nsi.txt', 'Zephium-Subscription-Trial-Windows-x64-setup.exe')
  $manifest = @(Get-Content (Join-Path $artifact 'SHA256SUMS'))
  if ($manifest.Count -ne $expectedFiles.Count) { throw 'Unexpected staged hash manifest.' }
  $seen = @{}
  foreach ($line in $manifest) {
    if ($line -cnotmatch '^([a-f0-9]{64})  ([A-Za-z0-9._-]+)$') { throw 'Malformed artifact hash entry.' }
    $hash = $Matches[1]; $name = $Matches[2]
    if ($expectedFiles -cnotcontains $name -or $seen.ContainsKey($name)) { throw 'Unexpected/duplicate artifact file.' }
    $path = Join-Path $artifact $name; Assert-NoReparse $path
    if ((Get-Hash $path) -cne $hash) { throw 'Staged artifact hash mismatch.' }; $seen[$name] = $true
  }
  if (@(Get-ChildItem -LiteralPath $artifact -Force).Count -ne ($expectedFiles.Count + 1)) { throw 'Unexpected extra artifact files.' }
  $installer = Join-Path $artifact 'Zephium-Subscription-Trial-Windows-x64-setup.exe'
  if ((Get-Hash $installer) -cne $ExpectedInstallerSha256) { throw 'Expected installer hash mismatch.' }
  $info = Get-Content (Join-Path $artifact 'BUILD-INFO.json') -Raw | ConvertFrom-Json
  Assert-TrialSmokeBuildInfo $info $context
  $signature = (Get-AuthenticodeSignature -LiteralPath $installer).Status.ToString()
  if ($signature -cne $info.authenticodeStatus -or $signature -cnotin @('NotSigned', 'Valid')) { throw 'Installer signature status changed or requires security review.' }
  Assert-TrialInstallerIdentity -Script (Get-Content (Join-Path $artifact 'installer.nsi.txt') -Raw)
  if ((Get-Hash (Join-Path $artifact 'installer-hooks.nsh.txt')) -cne (Get-Hash 'desktop/windows/subscription-trial-hooks.nsh')) { throw 'Staged hooks differ from the reviewed source.' }
  $builtBinary = Join-Path $root 'target/release/zephium-subscription-trial.exe'
  Assert-TrialInstallerBinarySource -Script (Get-Content (Join-Path $artifact 'installer.nsi.txt') -Raw) -ExpectedPath $builtBinary
  $binaryIdentity = Get-TrialBinaryIdentity -Path $builtBinary
  Assert-TrialBinaryIdentityInfo -Info $info -Identity $binaryIdentity
  $binaryHash = $binaryIdentity.NsisSha256
  Write-Output 'PASS: generated NSIS payload source is the exact built trial executable.'
  if ($UpgradeFromDeliveredTrial) {
    Assert-DistinctTrialUpgrade -Candidate $info -InstallerSha256 $ExpectedInstallerSha256
    $baseline = Expand-DeliveredTrialBaseline
  }

  $profileRoot = [Environment]::GetFolderPath('UserProfile')
  $local = [Environment]::GetFolderPath('LocalApplicationData')
  $roaming = [Environment]::GetFolderPath('ApplicationData')
  $desktop = [Environment]::GetFolderPath('DesktopDirectory')
  $programs = [Environment]::GetFolderPath('Programs')
  foreach ($path in @($local, $roaming, $desktop, $programs, $env:RUNNER_TEMP)) {
    if (-not $path -or -not (Test-Path -LiteralPath $path -PathType Container)) { throw 'Missing runner-owned fixture parent.' }
    Assert-NoReparse $path
  }
  foreach ($path in @($local, $roaming, $desktop, $programs)) {
    if (-not ([IO.Path]::GetFullPath($path)).StartsWith(($profileRoot.TrimEnd('\') + '\'), [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture path is outside the disposable runner profile.' }
  }
  $trialDirectory = Join-Path $local 'Zephium Subscription Trial'
  $productionDirectory = Join-Path $local 'Zephium'
  $trialData = Join-Path $roaming 'app.zephium.subscription-trial'
  $trialLocalData = Join-Path $local 'app.zephium.subscription-trial'
  $productionRoots = @($productionDirectory, (Join-Path $roaming 'app.zephium'), (Join-Path $local 'app.zephium'))
  $productionShortcuts = @((Join-Path $programs 'Zephium.lnk'), (Join-Path $desktop 'Zephium.lnk'))
  $trialShortcuts = @((Join-Path $programs 'Zephium Subscription Trial/Zephium Subscription Trial.lnk'), (Join-Path $desktop 'Zephium Subscription Trial.lnk'))
  foreach ($path in @($productionRoots) + $productionShortcuts + $trialShortcuts + @(
    $trialDirectory, $trialData, $trialLocalData, (Join-Path $programs 'Zephium'),
    (Join-Path $programs 'Zephium Subscription Trial'), (Join-Path $local 'Programs/Zephium'),
    (Join-Path $profileRoot '.zephium-native-v1'), (Join-Path $env:ProgramFiles 'Zephium'),
    (Join-Path ${env:ProgramFiles(x86)} 'Zephium'), (Join-Path $env:ProgramFiles 'Zephium Subscription Trial')
  )) { Assert-Absent $path }
  Assert-NoAppProcess
  $runKey = 'Software\Microsoft\Windows\CurrentVersion\Run'
  $approvedKey = 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
  $productionKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\Zephium'
  $trialKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\Zephium Subscription Trial'
  $absentKeys = @($productionKey, $trialKey, 'Software\Zephium', 'Software\Clients\StartMenuInternet\Zephium', 'Software\Classes\ZephiumURL', 'Software\Proma-code (experimental fork)\Zephium Subscription Trial')
  foreach ($key in $absentKeys) {
    if ((Read-RegistryTree ([Microsoft.Win32.Registry]::CurrentUser) $key) -cne 'absent') { throw 'Existing product registry state detected.' }
  }
  foreach ($view in @([Microsoft.Win32.RegistryView]::Registry32, [Microsoft.Win32.RegistryView]::Registry64)) {
    $machine = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
    try { foreach ($key in @($productionKey, $trialKey)) {
      if ((Read-RegistryTree $machine $key) -cne 'absent') { throw 'Machine-wide product installation detected.' }
    } } finally { $machine.Dispose() }
  }
  foreach ($key in @($runKey, $approvedKey)) { foreach ($name in @('Zephium', 'Zephium Subscription Trial')) {
    if ((Read-RegistryValue $key $name) -cne 'absent') { throw 'Existing startup state detected.' }
  } }
  if ((Read-RegistryValue 'Software\RegisteredApplications' 'Zephium') -cne 'absent') { throw 'Existing production browser registration detected.' }

  # Read-only browser snapshots stay in memory; no association fixture is written.
  $browserKeys = @('Software\RegisteredApplications', 'Software\Clients\StartMenuInternet\Zephium', 'Software\Classes\ZephiumURL', 'Software\Classes\http', 'Software\Classes\https',
    'Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice', 'Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice')
  $browserBefore = @{}; foreach ($key in $browserKeys) { $browserBefore[$key] = Read-RegistryTree ([Microsoft.Win32.Registry]::CurrentUser) $key }
  foreach ($path in $productionRoots) { New-FixtureDirectory $path; Save-FixtureFile (Join-Path $path 'preservation-fixture.txt') "synthetic:$fixtureId" }
  $productionExe = Join-Path $productionDirectory 'zephium-desktop.exe'
  Save-FixtureFile $productionExe "Not an executable. Synthetic preservation fixture $fixtureId."
  $shell = New-Object -ComObject WScript.Shell
  foreach ($path in $productionShortcuts) {
    $shortcut = $shell.CreateShortcut($path); $shortcut.TargetPath = $productionExe; $shortcut.Description = "Synthetic fixture $fixtureId"; $shortcut.Save()
    $fixtureFiles.Add(@{ path = $path; hash = (Get-Hash $path) })
  }
  Save-FixtureValue $productionKey 'DisplayName' 'Zephium' String
  Save-FixtureValue $productionKey 'FixtureOwner' $fixtureId String
  Save-FixtureValue $runKey 'Zephium' "synthetic-fixture:$fixtureId" String
  Save-FixtureValue $approvedKey 'Zephium' ([byte[]](3,0,0,0,0,0,0,0,0,0,0,0)) Binary
  $productionFileSnapshot = @($fixtureFiles.ToArray())
  $productionValueSnapshot = @($fixtureValues.ToArray())
  $productionRegistration = Read-RegistryTree ([Microsoft.Win32.Registry]::CurrentUser) $productionKey
  function Assert-ProductionPreserved {
    foreach ($entry in $productionFileSnapshot) {
      Assert-NoReparse $entry.path
      if (-not (Test-Path -LiteralPath $entry.path) -or (Get-Hash $entry.path) -cne $entry.hash) { throw 'Synthetic production file/shortcut changed.' }
    }
    foreach ($entry in $productionValueSnapshot) {
      if ((Read-RegistryValue $entry.path $entry.name) -cne $entry.snapshot) { throw 'Synthetic production registry value changed.' }
    }
    foreach ($key in $browserKeys) {
      if ((Read-RegistryTree ([Microsoft.Win32.Registry]::CurrentUser) $key) -cne $browserBefore[$key]) { throw 'Browser registration/default-choice snapshot changed.' }
    }
    if ((Read-RegistryTree ([Microsoft.Win32.Registry]::CurrentUser) $productionKey) -cne $productionRegistration) { throw 'Synthetic production uninstall registration changed.' }
    foreach ($view in @([Microsoft.Win32.RegistryView]::Registry32, [Microsoft.Win32.RegistryView]::Registry64)) {
      $machine = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
      try { foreach ($key in @($productionKey, $trialKey)) {
        if ((Read-RegistryTree $machine $key) -cne 'absent') { throw 'Unexpected machine-wide installation state.' }
      } } finally { $machine.Dispose() }
    }
    Assert-NoAppProcess
  }
  function Assert-TrialInstalled([string] $ExpectedPayloadHash = $binaryHash) {
    $exe = Join-Path $trialDirectory 'zephium-subscription-trial.exe'; Assert-NoReparse $exe
    $installedHash = Get-Hash $exe
    $installedProduct = (Get-Item -LiteralPath $exe).VersionInfo.ProductName
    $hashMatches = $installedHash -ceq $ExpectedPayloadHash
    $productMatches = $installedProduct -ceq 'Zephium Subscription Trial'
    Write-Output "Installed trial identity: bytes_match=$hashMatches; product_match=$productMatches; product_length=$(([string]$installedProduct).Length)."
    if (-not $hashMatches) { Write-Output "Expected NSIS SHA256=$ExpectedPayloadHash; installed SHA256=$installedHash." }
    Assert-TrialInstalledBinary -ActualHash $installedHash -ExpectedHash $ExpectedPayloadHash -ProductName $installedProduct
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($trialKey, $false)
    if ($null -eq $key) { throw 'Missing trial uninstall registration.' }
    try {
      if ($key.GetValue('DisplayName') -cne 'Zephium Subscription Trial' -or $key.GetValue('InstallLocation') -cne ('"' + $trialDirectory + '"')) { throw 'Trial uninstall identity mismatch.' }
    } finally { $key.Dispose() }
    foreach ($path in $trialShortcuts) {
      Assert-NoReparse $path
      if (-not (Test-Path -LiteralPath $path) -or $shell.CreateShortcut($path).TargetPath -ine $exe) { throw 'Trial shortcut target mismatch.' }
    }
  }
  $temporary = Join-Path $env:RUNNER_TEMP "zephium-trial-smoke-$fixtureId"
  New-FixtureDirectory $temporary
  function Remove-TrialInstallation {
    if (-not $uninstallerHash -or (Get-Hash $uninstaller) -cne $uninstallerHash) { throw 'Refusing unverified uninstaller.' }
    $copy = Join-Path $temporary 'uninstall.exe'
    if (-not (Test-Path -LiteralPath $copy)) {
      Copy-Item -LiteralPath $uninstaller -Destination $copy
      $fixtureFiles.Add(@{ path = $copy; hash = $uninstallerHash })
    }
    if ((Get-Hash $copy) -cne $uninstallerHash) { throw 'Copied uninstaller identity mismatch.' }
    # _?= must be last and unquoted for NSIS; fixed runner-owned path, no user input.
    Invoke-InstallerProcess $copy ("/S _?=$trialDirectory") $uninstallerHash
  }
  if ($UpgradeFromDeliveredTrial) {
    Invoke-InstallerProcess $baseline.InstallerPath '/S' $baseline.InstallerSha256
    Assert-TrialInstalled -ExpectedPayloadHash $baseline.PayloadSha256
  } else {
    Invoke-InstallerProcess $installer '/S' $ExpectedInstallerSha256
    Assert-TrialInstalled
  }
  $uninstaller = Join-Path $trialDirectory 'uninstall.exe'; Assert-NoReparse $uninstaller; $uninstallerHash = Get-Hash $uninstaller
  Assert-ProductionPreserved
  Write-Output 'PASS: silent trial install; synthetic production files, registry, shortcuts and browser choices preserved. App not launched.'
  New-FixtureDirectory $trialData; Save-FixtureFile (Join-Path $trialData 'retained-fixture.txt') "synthetic:$fixtureId"
  New-FixtureDirectory $trialLocalData; Save-FixtureFile (Join-Path $trialLocalData 'retained-fixture.txt') "synthetic:$fixtureId"
  Save-FixtureValue $runKey 'Zephium Subscription Trial' "synthetic-fixture:$fixtureId" String
  Save-FixtureValue $approvedKey 'Zephium Subscription Trial' ([byte[]](3,0,0,0,0,0,0,0,0,0,0,0)) Binary
  if ($UpgradeFromDeliveredTrial) {
    Save-FixtureFile (Join-Path $trialData 'upgrade-smoke-settings.json') (@{ fixture = $fixtureId; synthetic = $true; theme = 'dark'; note = 'Installer preservation fixture, not application DB or account state.' } | ConvertTo-Json -Compress)
    Add-Type -Path "$PSScriptRoot/windows-trial-upgrade-credential.cs"
    $upgradeCredential = [ZephiumTrialUpgradeCredential]::Create()
    if (-not $upgradeCredential.IsUnchanged()) { throw 'Synthetic credential fixture was not established.' }
    Invoke-InstallerProcess $installer '/S /UPDATE' $ExpectedInstallerSha256
    Assert-TrialInstalled; Assert-ProductionPreserved
    foreach ($entry in $fixtureFiles) { if ((Get-Hash $entry.path) -cne $entry.hash) { throw 'Different-build replacement changed a fixture.' } }
    foreach ($entry in $fixtureValues) { if ((Read-RegistryValue $entry.path $entry.name) -cne $entry.snapshot) { throw 'Different-build replacement changed fixture startup state.' } }
    if (-not $upgradeCredential.IsUnchanged()) { throw 'Different-build replacement changed the exact synthetic credential fixture.' }
    $uninstallerHash = Get-Hash $uninstaller
    Write-Output "PASS: delivered c4956618 -> $($info.sourceRevision) different-build replacement preserved synthetic settings-file/credential/startup and production sentinels; installed payload identity verified."
  }
  Invoke-InstallerProcess $installer '/S /UPDATE' $ExpectedInstallerSha256
  Assert-TrialInstalled; Assert-ProductionPreserved
  foreach ($entry in $fixtureFiles) { if ((Get-Hash $entry.path) -cne $entry.hash) { throw 'In-place reinstall changed a fixture.' } }
  foreach ($entry in $fixtureValues) { if ((Read-RegistryValue $entry.path $entry.name) -cne $entry.snapshot) { throw 'In-place reinstall changed fixture startup state.' } }
  if ($null -ne $upgradeCredential -and -not $upgradeCredential.IsUnchanged()) { throw 'Same-artifact reinstall changed the synthetic credential fixture.' }
  $uninstallerHash = Get-Hash $uninstaller
  Write-Output 'PASS: same-artifact in-place reinstall retained synthetic trial data/startup state; production sentinels preserved.'
  Remove-TrialInstallation
  $uninstalled = $true
  Assert-ProductionPreserved
  foreach ($path in @($trialDirectory) + $trialShortcuts) {
    if (Test-Path -LiteralPath $path) { throw 'Trial installed file/shortcut remains after uninstall.' }
  }
  if ((Read-RegistryTree ([Microsoft.Win32.Registry]::CurrentUser) $trialKey) -cne 'absent') { throw 'Trial uninstall registration remains.' }
  foreach ($key in @($runKey, $approvedKey)) {
    if ((Read-RegistryValue $key 'Zephium Subscription Trial') -cne 'absent') { throw 'Trial startup fixture remains after uninstall.' }
  }
  foreach ($entry in $fixtureFiles) { if ((Get-Hash $entry.path) -cne $entry.hash) { throw 'Ordinary uninstall removed retained fixture data.' } }
  if ($null -ne $upgradeCredential -and -not $upgradeCredential.IsUnchanged()) { throw 'Ordinary uninstall changed the synthetic credential fixture.' }
  Write-Output 'PASS: silent uninstall removed trial registration/shortcuts/startup values, retained synthetic trial data, preserved production sentinels.'
  Write-Output 'LIMIT: synthetic installer preservation only; no real production app, app launch, UI, real credentials, account flow, database migration or higher-version semantics were tested.'
} finally {
  # Never remove unknown trees or restore pre-existing state. A partial/unverified
  # installation is left to disposable-runner teardown, not guessed cleanup.
  try {
    if (-not $script:cleanupUnsafe -and -not $uninstalled -and $uninstallerHash -and (Get-Command Remove-TrialInstallation -ErrorAction SilentlyContinue)) {
      try { Remove-TrialInstallation } catch { Write-Warning 'Verified uninstall cleanup did not finish; leave state for disposable-runner teardown.' }
    }
    if ($script:cleanupUnsafe) {
      Write-Warning 'Process custody is uncertain: no further installer or fixture cleanup attempted. Dispose of this hosted runner.'
    } else {
      try { if ($null -ne $upgradeCredential) { $upgradeCredential.RemoveOwned() } }
      finally { Remove-OwnedFixtures }
    }
  } finally {
    Pop-Location
  }
}
