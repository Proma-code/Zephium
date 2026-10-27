# Credential-free managed-profile and adjacent-runtime qualification.
# Original images never log in or start a turn; owner transcripts are fixed synthetic helpers.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or $env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or $env:GITHUB_REPOSITORY -cne 'Proma-code/Zephium' -or $env:GITHUB_REF -cne 'refs/heads/windows/subscription-accounts') {
    throw 'Codex qualification requires the owned hosted Windows trial workflow.'
}
if ($env:GITHUB_RUN_ID -cnotmatch '\A[1-9][0-9]*\z' -or $env:GITHUB_RUN_ATTEMPT -cnotmatch '\A[1-9][0-9]*\z') { throw 'Invalid workflow identity.' }
function Assert-NoReparse([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path)
    while ($current) {
        if (Test-Path -LiteralPath $current) {
            $item=Get-Item -LiteralPath $current -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Reparse path refused.' }
        }
        $parent=[IO.Path]::GetDirectoryName($current)
        if ($parent -ceq $current) { break }
        $current=$parent
    }
}
$temp=[IO.Path]::GetFullPath($env:RUNNER_TEMP)
if (-not [IO.Path]::IsPathFullyQualified($temp) -or $temp.StartsWith('\\')) { throw 'Invalid runner temporary root.' }
Assert-NoReparse $temp
$root=Join-Path $temp "zephium-codex-managed-$env:GITHUB_RUN_ID-$env:GITHUB_RUN_ATTEMPT"
if (Test-Path -LiteralPath $root) { throw 'Managed qualification output already exists.' }
[IO.Directory]::CreateDirectory($root) | Out-Null
$fixture=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/debug/codex-broker-fixture.exe'))
Assert-NoReparse $fixture
if (-not (Test-Path -LiteralPath $fixture -PathType Leaf)) { throw 'Debug fixture is missing.' }
function Assert-Receipts([string[]]$Output,[string[]]$Markers) {
    foreach ($marker in $Markers) {
        if (@($Output | Where-Object { $_ -ceq "$($marker): native-code=0" }).Count -ne 1) {
            throw 'Required fixed managed qualification receipt missing or repeated.'
        }
    }
}
$output=@(& $fixture owner); $code=$LASTEXITCODE; $output | Write-Output
if ($code -ne 0) { throw 'Synthetic owned conversation qualification failed.' }
Assert-Receipts $output @(
    'codex-owner-two-turns-cold-resume',
    'codex-owner-stored-thread-cold-resume',
    'codex-owner-cancel-terminal',
    'codex-owner-invalidation-no-replay',
    'codex-owner-cancel-before-turn',
    'codex-owner-revision-before-write',
    'codex-owner-known-terminal-lost-ack',
    'codex-owner-early-terminal-lost-ack',
    'codex-owner-unsent-before-actor-continuity',
    'codex-owner-unsent-preparation-continuity',
    'codex-owner-unsent-writer-continuity',
    'codex-owner-unsent-bind-ack-unknown',
    'codex-owner-unsent-begin-ack-unknown',
    'codex-owner-unsent-skip-ack-unknown',
    'codex-owner-preparation-ack-unknown'
)
$output=@(& $fixture browser-owner); $code=$LASTEXITCODE; $output | Write-Output
if ($code -ne 0) { throw 'Synthetic Browser actor qualification failed.' }
Assert-Receipts $output @(
    'codex-browser-fixture-session-mode-bound',
    'codex-browser-two-turns-same-thread',
    'codex-browser-cold-store-same-thread',
    'codex-browser-input-cancel-skip-next-turn',
    'codex-browser-terminal-revocation-local-drain',
    'codex-browser-local-receipt-loss-quarantined',
    'codex-browser-duplicate-request-no-effect'
)
$output=@(& $fixture dormant-owner); $code=$LASTEXITCODE; $output | Write-Output
if ($code -ne 0) { throw 'Synthetic healthy dormant-owner qualification failed.' }
Assert-Receipts $output @(
    'codex-dormant-package-release-same-thread-wake',
    'codex-dormant-nonzero-not-healthy',
    'codex-dormant-late-frame-not-healthy',
    'codex-dormant-forced-close-not-healthy',
    'codex-dormant-expired-quota-late-reply-inert',
    'codex-dormant-expired-quota-absent-unknown',
    'codex-dormant-quota-duplicate-refused',
    'codex-dormant-quota-foreign-refused',
    'codex-dormant-quota-account-change-refused'
)
$output=@(& $fixture quota); $code=$LASTEXITCODE; $output | Write-Output
if ($code -ne 0) { throw 'Synthetic observational quota qualification failed.' }
Assert-Receipts $output @(
    'codex-quota-account-replacement-late-reply-ignored',
    'codex-quota-same-pump-account-replacement-coalesced',
    'codex-quota-provider-refusal-turn-settled',
    'codex-quota-expired-read-late-reply-turn-settled',
    'codex-quota-malformed-reading-turn-settled'
)
$output=@(& $fixture removal); $code=$LASTEXITCODE; $output | Write-Output
if ($code -ne 0) { throw 'Synthetic explicit removal settlement qualification failed.' }
Assert-Receipts $output @(
    'codex-removal-signedout-still-logout-same-lease',
    'codex-removal-pending-catalog-drained-no-pagination',
    'codex-removal-pending-quota-drained-before-settlement',
    'codex-removal-ack-without-notification-refused',
    'codex-removal-inflight-invalidation-refused',
    'codex-removal-late-invalidation-refused',
    'codex-removal-nonzero-exit-refused',
    'codex-removal-cancel-during-finish-refused'
)
$output=@(& $fixture vault); $code=$LASTEXITCODE; $output | Write-Output
if ($code -ne 0) { throw 'Nonsecret OS credential-store availability check failed.' }
Assert-Receipts $output @('codex-vault-nonsecret-roundtrip')
function Save-Original([hashtable]$Pin,[string]$Destination) {
    $handler=[Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect=$true
    $handler.MaxAutomaticRedirections=5
    $client=[Net.Http.HttpClient]::new($handler)
    $client.Timeout=[TimeSpan]::FromMinutes(3)
    $response=$null; $source=$null; $target=$null
    $cancel=[Threading.CancellationTokenSource]::new([TimeSpan]::FromMinutes(3))
    try {
        $uri="https://github.com/openai/codex/releases/download/rust-v$($Pin.Version)/$($Pin.Name)"
        $response=$client.GetAsync($uri,[Net.Http.HttpCompletionOption]::ResponseHeadersRead,$cancel.Token).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode() | Out-Null
        if ($response.Content.Headers.ContentLength -ne $Pin.Size) { throw 'Original image length mismatch.' }
        $source=$response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $target=[IO.FileStream]::new($Destination,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        $buffer=[byte[]]::new(65536); $total=0L
        $deadline=[DateTime]::UtcNow.AddMinutes(3)
        while (($read=$source.ReadAsync($buffer,0,$buffer.Length,$cancel.Token).GetAwaiter().GetResult()) -gt 0) {
            $total+=$read
            if ($total -gt $Pin.Size -or [DateTime]::UtcNow -ge $deadline) { throw 'Original image download exceeded its bound.' }
            $target.Write($buffer,0,$read)
        }
        if ($total -ne $Pin.Size) { throw 'Original image incomplete.' }
        $target.Flush($true); $target.Dispose(); $target=$null
        if ((Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant() -cne $Pin.Hash) { throw 'Original image digest mismatch.' }
    } finally {
        if ($target) { $target.Dispose() }; if ($source) { $source.Dispose() }; if ($response) { $response.Dispose() }; $client.Dispose(); $handler.Dispose(); $cancel.Dispose()
    }
}

# Original adjacent releases; image digests independently verified against the
# official release metadata. Neither version is installed or given an account.
$pins=@(
    @{Version='0.163.0';Name='codex-x86_64-pc-windows-msvc.exe';Size=341270320L;Hash='2692c8c7bb2cf8b0aefe638741fc90236f4b824c59578ce84994f9de58200993'},
    @{Version='0.164.0';Name='codex-x86_64-pc-windows-msvc.exe';Size=354558768L;Hash='6f267830b029ee25d9339864aa867cd45de9f8fd4c5ea007d2e63c13a0c66d36'}
)
foreach ($pin in $pins) {
    $path=Join-Path $root "codex-$($pin.Version).exe"
    Save-Original $pin $path
    $output=@(& $fixture compatibility $pin.Version $path)
    $code=$LASTEXITCODE
    $output | Write-Output
    if ($code -ne 0) { throw 'Original adjacent runtime compatibility qualification failed.' }
    Assert-Receipts $output @("codex-compatibility-$($pin.Version)")
    $output=@(& $fixture browser-schema $pin.Version $path)
    $code=$LASTEXITCODE
    $output | Write-Output
    if ($code -ne 0) { throw 'Original fixed experimental Browser schema qualification failed.' }
    Assert-Receipts $output @("codex-browser-schema-$($pin.Version)")
}
# This proves startup/config/account absence and physical teardown for both
# releases plus same-image experimental Browser schema export/cleanup, not real
# login, model responses, Page effects or cross-version authenticated turns.

$output=@(& $fixture dormant-original (Join-Path $root 'codex-0.163.0.exe') (Join-Path $root 'codex-0.164.0.exe'))
$code=$LASTEXITCODE; $output | Write-Output
if ($code -ne 0) { throw 'Original credential-free dormant image/home wake qualification failed.' }
Assert-Receipts $output @('codex-dormant-original-0.163.0-to-0.164.0')
# The original pair proves fresh authenticated image admission, package-pin
# release, unchanged managed home/grant and fresh signed-out account read only.
# Same-thread continuity is synthetic above; no authenticated original turn runs.
