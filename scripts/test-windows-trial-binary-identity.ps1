$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/windows-trial-binary-identity.ps1"
. "$PSScriptRoot/windows-trial-install-checks.ps1"
Add-Type -Path "$PSScriptRoot/windows-trial-binary-identity.cs"
function Assert-Equal($Actual, $Expected) {
  if ($Actual -cne $Expected) { throw 'Binary identity fixture mismatch.' }
}
function Assert-Rejected([scriptblock] $Check) {
  $failed = $false; try { & $Check } catch { $failed = $true }
  if (-not $failed) { throw 'Ambiguous or changed binary identity was accepted.' }
}
function Hash-Bytes([byte[]] $Bytes) { [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant() }
$prefix = [byte[]](0, 1, 2, 255, 0, 128)
$suffix = [byte[]](255, 254, 253, 0, 1)
$marker = [Text.Encoding]::ASCII.GetBytes('__TAURI_BUNDLE_TYPE_VAR_UNK')
$patched = [Text.Encoding]::ASCII.GetBytes('__TAURI_BUNDLE_TYPE_VAR_NSS')
$source = [byte[]]($prefix + $marker + $suffix)
$expected = [byte[]]($prefix + $patched + $suffix)
$before = Hash-Bytes $source
$identity = [ZephiumTrialBinaryIdentity]::InspectBytes($source)
Assert-Equal $identity.CompilerSha256 $before
Assert-Equal $identity.NsisSha256 (Hash-Bytes $expected)
Assert-Equal $identity.Length $source.Length
Assert-Equal $identity.MarkerOffset $prefix.Length
Assert-Equal (Hash-Bytes $source) $before
Assert-TrialInstalledBinary (Hash-Bytes $expected) $identity.NsisSha256 'Zephium Subscription Trial'
# The unpatched compiler file is NOT an acceptable installed NSIS payload.
Assert-Rejected { Assert-TrialInstalledBinary $identity.CompilerSha256 $identity.NsisSha256 'Zephium Subscription Trial' }
foreach ($bad in @([byte[]]@(), [byte[]]$patched, [byte[]]($marker + $marker), [byte[]]($prefix + $marker[0..($marker.Length - 2)] + $suffix))) {
  Assert-Rejected { [ZephiumTrialBinaryIdentity]::InspectBytes($bad) }
}
foreach ($position in @(0, ($expected.Length - 1), $prefix.Length)) {
  $mutated = [byte[]]$expected.Clone(); $mutated[$position] = $mutated[$position] -bxor 1
  Assert-Rejected { Assert-TrialInstalledBinary (Hash-Bytes $mutated) $identity.NsisSha256 'Zephium Subscription Trial' }
}
$info = @{
  compilerExecutableSha256 = $identity.CompilerSha256; nsisExecutableSha256 = $identity.NsisSha256
  executableBytes = $identity.Length; bundleIdentityMethod = 'tauri-cli-2.11.4-unsigned-nsis'
}
Assert-TrialBinaryIdentityInfo $info $identity
foreach ($key in $info.Keys) {
  $badInfo = $info.Clone(); $badInfo[$key] = 'changed'
  Assert-Rejected { Assert-TrialBinaryIdentityInfo $badInfo $identity }
}
$temp = Join-Path ([IO.Path]::GetTempPath()) ('zephium-identity-fixture-' + [Guid]::NewGuid().ToString('N'))
$file = [IO.FileStream]::new($temp, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
try { $file.Write($source); $file.Flush($true) } finally { $file.Dispose() }
try {
  $fromFile = Get-TrialBinaryIdentity -Path $temp
  Assert-Equal $fromFile.CompilerSha256 $identity.CompilerSha256
  Assert-Equal $fromFile.NsisSha256 $identity.NsisSha256
  Assert-Equal ((Get-FileHash -LiteralPath $temp -Algorithm SHA256).Hash.ToLowerInvariant()) $before
} finally {
  if ((Get-FileHash -LiteralPath $temp -Algorithm SHA256).Hash.ToLowerInvariant() -cne $before) {
    throw 'Synthetic file changed; leaving it untouched for inspection.'
  }
  Remove-Item -LiteralPath $temp
}
Write-Output 'Compiler/NSIS whole-file identity fixtures passed; no executable was patched or launched.'
