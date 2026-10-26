# Validate the actual generated NSIS script, not just the input configuration.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-staging-checks.ps1"
. "$PSScriptRoot/windows-trial-binary-identity.ps1"
if (-not $IsWindows) { throw 'Windows artifact staging must run on Windows.' }
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
  $revision = (git rev-parse HEAD).Trim()
  if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source revision.' }
  $sourceStatus = git status --porcelain --untracked-files=normal | Out-String
  if ($LASTEXITCODE -ne 0) { throw 'Cannot establish a clean source checkout.' }
  Assert-TrialSourceProvenance -Revision $revision -SourceStatus $sourceStatus -ExpectedSha "$env:GITHUB_SHA"
  $installers = @(Get-ChildItem 'target/release/bundle/nsis' -File -Filter 'Zephium Subscription Trial_*_x64-setup.exe')
  if ($installers.Count -ne 1) { throw 'Expected exactly one isolated x64 trial installer.' }
  $scripts = @(Get-ChildItem 'target/release/nsis' -File -Recurse -Filter installer.nsi)
  if ($scripts.Count -ne 1) { throw 'Expected one generated NSIS script to verify.' }
  $nsi = Get-Content $scripts[0].FullName -Raw
  Assert-TrialInstallerIdentity -Script $nsi
  $exe = Get-Item 'target/release/zephium-subscription-trial.exe'
  if ($exe.VersionInfo.ProductName -ne 'Zephium Subscription Trial') { throw 'Executable product name is not isolated.' }
  if ((Get-AuthenticodeSignature -LiteralPath $exe.FullName).Status.ToString() -cne 'NotSigned') { throw 'Expected an unsigned compiler executable for the reviewed NSIS identity derivation.' }
  $binaryIdentity = Get-TrialBinaryIdentity -Path $exe.FullName
  $out = (Get-TrialOutputPaths).ArtifactDirectory
  if (Test-Path -LiteralPath $out) { throw 'Artifact staging directory already exists for this run/attempt; inspect it before retrying.' }
  New-Item -ItemType Directory $out | Out-Null
  $setupName = 'Zephium-Subscription-Trial-Windows-x64-setup.exe'
  Copy-Item $installers[0].FullName (Join-Path $out $setupName)
  Copy-Item 'docs/windows-subscription-trial.md' (Join-Path $out 'README.md')
  Copy-Item $scripts[0].FullName (Join-Path $out 'installer.nsi.txt')
  Copy-Item 'desktop/windows/subscription-trial-hooks.nsh' (Join-Path $out 'installer-hooks.nsh.txt')
  $signature = Get-AuthenticodeSignature $installers[0].FullName
  @{
    sourceRevision = $revision
    sourceState = 'clean'
    workflowRunId = "$env:GITHUB_RUN_ID"
    workflowRunAttempt = "$env:GITHUB_RUN_ATTEMPT"
    identifier = 'app.zephium.subscription-trial'
    productName = 'Zephium Subscription Trial'
    executable = $exe.Name
    compilerExecutableSha256 = $binaryIdentity.CompilerSha256
    nsisExecutableSha256 = $binaryIdentity.NsisSha256
    executableBytes = $binaryIdentity.Length
    bundleIdentityMethod = 'tauri-cli-2.11.4-unsigned-nsis'
    installer = $setupName
    authenticodeStatus = $signature.Status.ToString()
    createdUtc = [DateTime]::UtcNow.ToString('o')
    verification = 'Staging checked clean source identity, generated installer identity, executable product name and artifact hashes. Consult the exact workflow run for build/test results. Interactive side-by-side and account sign-in remain unverified.'
  } | ConvertTo-Json | Set-Content (Join-Path $out 'BUILD-INFO.json') -Encoding utf8
  $checksums = @(Get-ChildItem $out -File | Sort-Object Name | ForEach-Object {
    $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $($_.Name)"
  })
  $checksums | Set-Content (Join-Path $out 'SHA256SUMS') -Encoding ascii
  if ($env:GITHUB_STEP_SUMMARY) {
    @"
  ## Isolated Windows trial artifact
  - Source: ``$revision``
  - Product: **Zephium Subscription Trial**
  - Authenticode: **$($signature.Status)** (no signing key configured)
  - Installer config and executable product identity checked; SHA256SUMS included.
  - No release, updater metadata, deployment or PR created.
  - Interactive coexistence, uninstall preservation, WebView2 and live account flows are **not verified** by this build.
"@ | Add-Content $env:GITHUB_STEP_SUMMARY
  }
} finally {
  Pop-Location
}
