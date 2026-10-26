# Shared read-only compiler-versus-NSIS identity. Never patch the executable on disk.
function Get-TrialBinaryIdentity {
  param([Parameter(Mandatory)][string] $Path)
  $package = Get-Content (Join-Path $PSScriptRoot '../desktop/package.json') -Raw | ConvertFrom-Json
  if ($package.devDependencies.'@tauri-apps/cli' -cne '2.11.4') {
    throw 'Trial bundle identity must be reviewed for the configured Tauri CLI version.'
  }
  if (-not ('ZephiumTrialBinaryIdentity' -as [type])) {
    Add-Type -Path (Join-Path $PSScriptRoot 'windows-trial-binary-identity.cs')
  }
  return [ZephiumTrialBinaryIdentity]::InspectFile($Path)
}

function Assert-TrialBinaryIdentityInfo {
  param([Parameter(Mandatory)] $Info, [Parameter(Mandatory)] $Identity)
  if ($Info.compilerExecutableSha256 -cne $Identity.CompilerSha256 -or
      $Info.nsisExecutableSha256 -cne $Identity.NsisSha256 -or
      $Info.executableBytes -ne $Identity.Length -or
      $Info.bundleIdentityMethod -cne 'tauri-cli-2.11.4-unsigned-nsis') {
    throw 'Staged compiler/NSIS executable identity differs from the exact build.'
  }
}
