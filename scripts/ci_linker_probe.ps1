$ErrorActionPreference = "Stop"
$probeTarget = Join-Path $env:RUNNER_TEMP "herdr-ci-lld"
$sysroot = (& rustc --print sysroot).Trim()
if ($LASTEXITCODE -ne 0) { throw "could not locate Rust toolchain" }
$lldLink = Join-Path $sysroot "lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe"
if (-not (Test-Path -LiteralPath $lldLink)) { throw "bundled rust-lld.exe not found" }
$results = [System.Collections.Generic.List[object]]::new()

function Set-Linker {
    param([string]$Linker)
    if ($Linker -eq "lld") {
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $lldLink
        $env:CARGO_TARGET_DIR = $probeTarget
    } else {
        Remove-Item Env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER -ErrorAction SilentlyContinue
        $env:CARGO_TARGET_DIR = Join-Path $PWD "target"
    }
}

function Invoke-Compile {
    param([string]$Linker, [string]$Phase)
    Set-Linker $Linker
    (Get-Item src/main.rs).LastWriteTimeUtc = [DateTime]::UtcNow
    $timer = [Diagnostics.Stopwatch]::StartNew()
    & cargo nextest run --locked --no-run
    $exitCode = $LASTEXITCODE
    $timer.Stop()
    if ($exitCode -ne 0) { throw "$Linker build failed with exit code $exitCode" }
    $seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 2)
    Write-Output "CI_LINKER_PROBE phase=$Phase linker=$Linker seconds=$seconds"
    $results.Add([pscustomobject]@{phase=$Phase; linker=$Linker; seconds=$seconds})
}

try {
    Invoke-Compile "lld" "warmup"
    foreach ($linker in @("lld", "msvc", "msvc", "lld")) {
        Invoke-Compile $linker "measured"
    }
    Set-Linker "lld"
    if (-not (Test-Path (Join-Path $probeTarget "debug\herdr.pdb"))) {
        throw "LLD did not produce Herdr debug symbols"
    }
    & cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    if ($LASTEXITCODE -ne 0) { throw "LLD-linked tests failed" }
    & ./scripts/windows_smoke_conpty_path.ps1 -ExePath (Join-Path $probeTarget "debug\herdr.exe") -Session "ci-lld-$env:GITHUB_RUN_ID-$env:GITHUB_RUN_ATTEMPT"
    if ($LASTEXITCODE -ne 0) { throw "LLD-linked ConPTY smoke failed" }
    $results | ConvertTo-Json | Out-File -FilePath $env:GITHUB_STEP_SUMMARY -Append
} finally {
    Remove-Item $probeTarget -Recurse -Force -ErrorAction SilentlyContinue
}
