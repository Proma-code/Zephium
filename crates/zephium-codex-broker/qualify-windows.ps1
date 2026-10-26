# Credential-free original runtime qualification. This never logs in or starts a turn.
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
$root=Join-Path $temp "zephium-codex-$env:GITHUB_RUN_ID-$env:GITHUB_RUN_ATTEMPT"
if (Test-Path -LiteralPath $root) { throw 'Qualification output already exists.' }
[IO.Directory]::CreateDirectory($root) | Out-Null
$fixture=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../target/debug/codex-broker-fixture.exe'))
Assert-NoReparse $fixture
if (-not (Test-Path -LiteralPath $fixture -PathType Leaf)) { throw 'Debug fixture is missing.' }
$control=@(& $fixture transport)
$code=$LASTEXITCODE
$control | Write-Output
if ($code -ne 0) { throw 'Synthetic transport qualification failed.' }
foreach ($marker in @('nonzero','truncated-eof','blocked-writer','owned-descendant')) {
    if (@($control | Where-Object { $_ -ceq "codex-transport-${marker}: native-code=0" }).Count -ne 1) { throw 'Required fixed transport receipt missing or repeated.' }
}
# Exact official release rust-v0.164.0 -> source6aaac694803004a63c6f393d153c96d22521a6b9.
$pins=@(
    @{Kind='cli';Name='codex-x86_64-pc-windows-msvc.exe';Size=354558768L;Hash='6f267830b029ee25d9339864aa867cd45de9f8fd4c5ea007d2e63c13a0c66d36'},
    @{Kind='standalone';Name='codex-app-server-x86_64-pc-windows-msvc.exe';Size=273012016L;Hash='82314aa236834a8b7240cc5fe4fdf71d4502822e476105ccbe010ef6689083d7'}
)
function Save-Original([hashtable]$Pin,[string]$Destination) {
    $handler=[Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect=$true
    $handler.MaxAutomaticRedirections=5
    $client=[Net.Http.HttpClient]::new($handler)
    $client.Timeout=[TimeSpan]::FromMinutes(3)
    $response=$null; $source=$null; $target=$null
    $cancel=[Threading.CancellationTokenSource]::new([TimeSpan]::FromMinutes(3))
    try {
        $uri="https://github.com/openai/codex/releases/download/rust-v0.164.0/$($Pin.Name)"
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
foreach ($pin in $pins) {
    $path=Join-Path $root $pin.Name
    Save-Original $pin $path
    $output=@(& $fixture $pin.Kind $path)
    $code=$LASTEXITCODE
    $output | Write-Output
    if ($code -ne 0) { throw "Fixed $($pin.Kind) qualification failed." }
    foreach ($marker in @('image-verified','owned-process','initialized','remote-disabled','signed-out','owned-config','requirements-owned-login-only','exit-zero-job-empty','runtime-cleaned','cancelled-reaped-cleaned')) {
        if (@($output | Where-Object { $_ -ceq "codex-stage=$marker; code=0" }).Count -ne 1) { throw 'Required fixed qualification receipt missing or repeated.' }
    }
    Write-Output "codex-original-kind=$($pin.Kind); qualification=passed"
}
# Downloaded original images are disposable runner output. No user installation is changed.
