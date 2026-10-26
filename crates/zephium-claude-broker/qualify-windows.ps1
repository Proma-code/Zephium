# Disposable Windows CI only. No installer, global config, account or inference.
# The exact release manifest was separately verified with Anthropic's documented
# signing-key fingerprint; native code independently checks pinned bytes and
# Authenticode chain/revocation/publisher before CreateProcessW.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:GITHUB_REPOSITORY -ne 'Proma-code/Zephium') { throw 'This qualification is restricted to the disposable ownfork Actions worker' }
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) { throw 'This image pin is Windows x64 only' }
if (-not $IsWindows -or -not (Test-Path -LiteralPath $env:RUNNER_TEMP -PathType Container)) { throw 'Disposable Windows runner temporary root is unavailable' }
$Version = '2.1.310'
$ExpectedHash = 'f9a3920634ae3b961ee92c2eb58d0051f5f8852b120983d0a4f57b6a5efeba61'
$ExpectedSize = 296791712
$Root = Join-Path $env:RUNNER_TEMP ('zephium-claude-image-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Root | Out-Null
try {
  cargo test -p zephium-claude-broker --locked
  if ($LASTEXITCODE -ne 0) { throw 'Portable broker tests failed' }
  cargo test -p zephium-claude-broker --locked --features native-fixtures --all-targets
  if ($LASTEXITCODE -ne 0) { throw 'Synthetic native research assertions failed' }
  cargo run -p zephium-claude-broker --locked --features native-fixtures --bin claude-broker-fixture -- token-controls
  if ($LASTEXITCODE -ne 0) { throw 'Actual suspended token ACL controls failed; runtime probe not admitted' }
  cargo run -p zephium-claude-broker --locked --features native-fixtures --bin claude-broker-fixture -- auth-inputs
  if ($LASTEXITCODE -ne 0) { throw 'Synthetic interactive input lifecycle failed; vendor probe not admitted' }
  cargo run -p zephium-claude-broker --locked --features native-fixtures --bin claude-broker-fixture -- network-controls
  if ($LASTEXITCODE -ne 0) { throw 'Synthetic differential network controls failed; vendor probe not admitted' }
  cargo run -p zephium-claude-broker --locked --features native-fixtures --bin claude-broker-fixture -- qualify
  if ($LASTEXITCODE -ne 0) { throw 'Synthetic native confinement failed; vendor probe not admitted' }
  $Image = Join-Path $Root 'claude.exe'
  $Handler = [System.Net.Http.HttpClientHandler]::new()
  $Handler.AllowAutoRedirect = $false
  $Client = [System.Net.Http.HttpClient]::new($Handler)
  $Client.Timeout = [TimeSpan]::FromSeconds(120)
  $Cancellation = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds(120))
  $Response = $null
  $Output = $null
  $InputStream = $null
  try {
    $Uri = "https://downloads.claude.ai/claude-code-releases/$Version/win32-x64/claude.exe"
    $Response = $Client.GetAsync($Uri, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead, $Cancellation.Token).GetAwaiter().GetResult()
    if ($Response.StatusCode -ne [System.Net.HttpStatusCode]::OK -or $Response.Content.Headers.ContentLength -ne $ExpectedSize) { throw 'Pinned vendor download response mismatch' }
    $InputStream = $Response.Content.ReadAsStreamAsync($Cancellation.Token).GetAwaiter().GetResult()
    $Output = [System.IO.File]::Open($Image, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    $Buffer = [byte[]]::new(65536)
    $Total = 0L
    $Deadline = [DateTime]::UtcNow.AddSeconds(120)
    while (($Count = $InputStream.ReadAsync($Buffer, 0, $Buffer.Length, $Cancellation.Token).GetAwaiter().GetResult()) -gt 0) {
      $Total += $Count
      if ($Total -gt $ExpectedSize -or [DateTime]::UtcNow -ge $Deadline) { throw 'Pinned vendor download exceeded bound' }
      $Output.Write($Buffer, 0, $Count)
    }
    if ($Total -ne $ExpectedSize) { throw 'Pinned vendor download incomplete' }
  } finally {
    if ($Output) { $Output.Dispose() }
    if ($InputStream) { $InputStream.Dispose() }
    if ($Response) { $Response.Dispose() }
    $Cancellation.Dispose()
    $Client.Dispose()
    $Handler.Dispose()
  }
  if ((Get-Item -LiteralPath $Image).Length -ne $ExpectedSize) { throw 'Pinned vendor image size mismatch' }
  if ((Get-FileHash -LiteralPath $Image -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ExpectedHash) { throw 'Pinned vendor image hash mismatch' }
  cargo run -p zephium-claude-broker --locked --features native-fixtures --bin claude-broker-fixture -- vendor-probe $Image
  if ($LASTEXITCODE -ne 0) { throw 'Pinned vendor startup remains unqualified' }
} finally {
  # Only the uniquely-created CI download directory; native fixture roots are owned
  # by the helper and cannot contain any real credentials under this contract.
  Remove-Item -LiteralPath $Root -Recurse -Force
}
