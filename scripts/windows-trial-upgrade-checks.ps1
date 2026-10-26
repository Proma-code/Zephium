# One reviewed delivered baseline only. No network or caller-supplied path.
function Get-DeliveredTrialPin {
  return [pscustomobject]@{
    Source = 'c4956618e1000b5909de351418fc574405cd0849'; Run = '40297504720'; Attempt = '1'; ArtifactId = '12640672752'
    ZipName = 'Zephium-Subscription-Trial-Windows-x64-c4956618.zip'
    ZipSha256 = '8ea2b2a39b2568b7e2acc9504afb2c3be048ef25a15fff6e1d27569fb46791d7'; ZipBytes = 15913690
    Installer = 'Zephium-Subscription-Trial-Windows-x64-setup.exe'
    InstallerSha256 = '4b19092418d7f131c815c8e112b0c2f4274675edb863148c91212c4bf657e55f'
    PayloadSha256 = '5ad0561738da517d042bcb8b3ddc5f6bf32065c8b3728acffbed94e5854db34f'; PayloadBytes = 44202496
    Files = @{
      'BUILD-INFO.json' = @('8758fc446e9dcd1a8447505d4167b74e6111021fdaaa8677ba44c378dfbb9c44', 1009)
      'installer.nsi.txt' = @('5b6f0c356792f9dea92f8edff21988c6289e70e4e9cdf05ace75d22b537a0502', 31939)
      'README.md' = @('cb6100aed0a1984537e2e84887af039369ea018bc37ca3a993535f28a507acda', 3379)
      'installer-hooks.nsh.txt' = @('e248732410623d303e6fa10d515fee4e92cb4f32090a753199588d6af93ac8de', 1594)
      'SHA256SUMS' = @('bdbe992b99f639776a2c01de3ae7b8c1776269b2108ff33bf4f78bb1143e9119', 452)
      'Zephium-Subscription-Trial-Windows-x64-setup.exe' = @('4b19092418d7f131c815c8e112b0c2f4274675edb863148c91212c4bf657e55f', 15915793)
    }
  }
}

function Assert-DeliveredArchiveCandidate {
  param([object[]] $Entries, [bool] $DestinationExists)
  $pin = Get-DeliveredTrialPin
  if ($Entries.Count -ne 1 -or $Entries[0].Directory -or $Entries[0].Reparse -or
      $Entries[0].Length -ne $pin.ZipBytes -or $Entries[0].Sha256 -cne $pin.ZipSha256) { throw 'Expected exactly one regular pinned delivered ZIP.' }
  if ($Entries[0].Name -cne $pin.ZipName -and $DestinationExists) { throw 'Refusing to overwrite the fixed upgrade ZIP destination.' }
}

function Assert-DeliveredArchiveEntries {
  param([object[]] $Entries)
  $pin = Get-DeliveredTrialPin; $seen = @{}
  if ($Entries.Count -ne $pin.Files.Count) { throw 'Delivered archive has unexpected entries.' }
  foreach ($entry in $Entries) {
    if (@($pin.Files.Keys) -cnotcontains $entry.Name -or $seen.ContainsKey($entry.Name) -or $entry.Directory -or $entry.Reparse) { throw 'Delivered archive entry is not an allowed flat regular file.' }
    $expected = $pin.Files[$entry.Name]
    if ($entry.Length -ne $expected[1] -or $entry.Sha256 -cne $expected[0]) { throw 'Delivered archive entry identity mismatch.' }
    $seen[$entry.Name] = $true
  }
}

function Assert-DeliveredTrialInfo {
  param($Info)
  $pin = Get-DeliveredTrialPin
  foreach ($entry in @{
    sourceRevision = $pin.Source; sourceState = 'clean'; workflowRunId = $pin.Run; workflowRunAttempt = $pin.Attempt
    identifier = 'app.zephium.subscription-trial'; productName = 'Zephium Subscription Trial'; executable = 'zephium-subscription-trial.exe'
    installer = $pin.Installer; nsisExecutableSha256 = $pin.PayloadSha256; executableBytes = $pin.PayloadBytes
    compilerExecutableSha256 = '42bf17218dc593dee90253b274d59951489b2a287f50a8727891cbcbf530e48c'
    bundleIdentityMethod = 'tauri-cli-2.11.4-unsigned-nsis'; authenticodeStatus = 'NotSigned'
  }.GetEnumerator()) {
    if ($Info.($entry.Key) -cne $entry.Value) { throw "Delivered trial identity mismatch: $($entry.Key)." }
  }
}

function Assert-DistinctTrialUpgrade {
  param($Candidate, [string] $InstallerSha256)
  $pin = Get-DeliveredTrialPin
  if ($Candidate.sourceRevision -cnotmatch '\A[a-f0-9]{40}\z' -or $Candidate.sourceRevision -ceq $pin.Source -or
      $InstallerSha256 -cnotmatch '\A[a-f0-9]{64}\z' -or $InstallerSha256 -ceq $pin.InstallerSha256 -or
      $Candidate.nsisExecutableSha256 -cnotmatch '\A[a-f0-9]{64}\z' -or $Candidate.nsisExecutableSha256 -ceq $pin.PayloadSha256) {
    throw 'Upgrade requires a different source, installer and installed payload from the delivered baseline.'
  }
}

function Get-DeliveredTrialUpgradePaths {
  if ($env:GITHUB_ACTIONS -cne 'true') { throw 'Delivered upgrade fixtures require the hosted workflow.' }
  $parent = Split-Path (Get-TrialOutputPaths).ArtifactDirectory -Parent
  $input = Join-Path $parent 'upgrade-input'
  return [pscustomobject]@{
    InputDirectory = $input
    ZipPath = Join-Path $input (Get-DeliveredTrialPin).ZipName
    ExtractDirectory = Join-Path $parent 'upgrade-baseline-c4956618'
  }
}

function Assert-UpgradeDirectPath([string] $Path) {
  $current = [IO.Path]::GetFullPath($Path)
  while ($current) {
    if ((Test-Path -LiteralPath $current) -and ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Upgrade paths must not use reparse points.' }
    $current = Split-Path $current -Parent
  }
}

function Initialize-DeliveredUpgradeArchive {
  $paths = Get-DeliveredTrialUpgradePaths
  Assert-UpgradeDirectPath $paths.InputDirectory
  $items = @(Get-ChildItem -LiteralPath $paths.InputDirectory -Force)
  $observed = @($items | ForEach-Object {
    $reparse = [bool]($_.Attributes -band [IO.FileAttributes]::ReparsePoint)
    $regular = -not $_.PSIsContainer -and -not $reparse
    [pscustomobject]@{ Name = $_.Name; Directory = $_.PSIsContainer; Reparse = $reparse
      Length = $(if ($regular) { $_.Length } else { -1 })
      Sha256 = $(if ($regular -and $_.Length -eq (Get-DeliveredTrialPin).ZipBytes) { (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() } else { '' }) }
  })
  Assert-DeliveredArchiveCandidate -Entries $observed -DestinationExists (Test-Path -LiteralPath $paths.ZipPath)
  if ($items[0].Name -cne (Get-DeliveredTrialPin).ZipName) {
    [IO.File]::Move($items[0].FullName, $paths.ZipPath) # Never overwrite.
  }
  return $paths.ZipPath
}

function Expand-DeliveredTrialBaseline {
  $paths = Get-DeliveredTrialUpgradePaths; $pin = Get-DeliveredTrialPin
  $zip = Initialize-DeliveredUpgradeArchive
  Assert-UpgradeDirectPath $paths.ExtractDirectory
  if (Test-Path -LiteralPath $paths.ExtractDirectory) { throw 'Delivered baseline extraction already exists; no stale output is deleted.' }
  $file = [IO.FileStream]::new($zip, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
  $archive = $null
  try {
    if ($file.Length -ne $pin.ZipBytes -or [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($file)).ToLowerInvariant() -cne $pin.ZipSha256) { throw 'Held delivered archive differs from its pin.' }
    $file.Position = 0
    $archive = [IO.Compression.ZipArchive]::new($file, [IO.Compression.ZipArchiveMode]::Read, $true)
    $observed = @(); $content = @{}
    foreach ($entry in $archive.Entries) {
      if (@($pin.Files.Keys) -cnotcontains $entry.FullName -or $content.ContainsKey($entry.FullName) -or $entry.Length -ne $pin.Files[$entry.FullName][1]) { throw 'Unrecognized or oversized delivered archive entry.' }
      $stream = $entry.Open()
      try {
        $buffer = [IO.MemoryStream]::new(); try { $stream.CopyTo($buffer); $bytes = $buffer.ToArray() } finally { $buffer.Dispose() }
      } finally { $stream.Dispose() }
      $content[$entry.FullName] = $bytes
      $observed += [pscustomobject]@{ Name = $entry.FullName; Length = $bytes.Length
        Sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
        Directory = [bool]($entry.ExternalAttributes -band 0x10)
        Reparse = (($entry.ExternalAttributes -shr 16) -band 0xf000) -eq 0xa000 }
    }
    Assert-DeliveredArchiveEntries $observed
    $info = [Text.Encoding]::UTF8.GetString($content['BUILD-INFO.json']).TrimStart([char]0xfeff) | ConvertFrom-Json
    Assert-DeliveredTrialInfo $info
    Assert-TrialInstallerIdentity -Script ([Text.Encoding]::UTF8.GetString($content['installer.nsi.txt']))
    $manifest = ([Text.Encoding]::UTF8.GetString($content['SHA256SUMS']).Trim() -split '\r?\n')
    $seen = @{}
    foreach ($line in $manifest) {
      if ($line -cnotmatch '\A([a-f0-9]{64})  ([A-Za-z0-9._-]+)\z' -or $Matches[2] -ceq 'SHA256SUMS' -or
          @($pin.Files.Keys) -cnotcontains $Matches[2] -or $seen.ContainsKey($Matches[2]) -or $Matches[1] -cne $pin.Files[$Matches[2]][0]) { throw 'Delivered internal checksum manifest mismatch.' }
      $seen[$Matches[2]] = $true
    }
    if ($seen.Count -ne 5) { throw 'Delivered checksum manifest is incomplete.' }
    New-Item -ItemType Directory -Path $paths.ExtractDirectory | Out-Null
    foreach ($name in $content.Keys) {
      $output = [IO.FileStream]::new((Join-Path $paths.ExtractDirectory $name), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
      try { $output.Write($content[$name], 0, $content[$name].Length) } finally { $output.Dispose() }
    }
  } finally { if ($null -ne $archive) { $archive.Dispose() }; $file.Dispose() }
  $installer = Join-Path $paths.ExtractDirectory $pin.Installer
  if ((Get-AuthenticodeSignature -LiteralPath $installer).Status.ToString() -cne 'NotSigned') { throw 'Delivered installer signature status differs from its reviewed pin.' }
  return [pscustomobject]@{ InstallerPath = $installer; InstallerSha256 = $pin.InstallerSha256; PayloadSha256 = $pin.PayloadSha256; Source = $pin.Source }
}
