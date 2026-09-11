$ErrorActionPreference = "Stop"
$probeTarget = Join-Path $env:RUNNER_TEMP "herdr-ci-debug2"
$results = [System.Collections.Generic.List[object]]::new()

function Invoke-ProfileBuild {
    param([string]$Level, [string]$Phase)

    $env:CARGO_PROFILE_DEV_DEBUG = $Level
    $env:CARGO_PROFILE_TEST_DEBUG = $Level
    $env:CARGO_TARGET_DIR = if ($Level -eq "1") { Join-Path $PWD "target" } else { $probeTarget }
    # Invalidate Herdr without changing source contents or rebuilding dependencies.
    (Get-Item src/main.rs).LastWriteTimeUtc = [DateTime]::UtcNow
    $timer = [Diagnostics.Stopwatch]::StartNew()
    & cargo nextest run --locked --no-run
    $exitCode = $LASTEXITCODE
    $timer.Stop()
    if ($exitCode -ne 0) {
        throw "debug profile $Level build failed with exit code $exitCode"
    }
    $seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 2)
    Write-Output "CI_DEBUG_PROBE phase=$Phase debug=$Level seconds=$seconds"
    $results.Add([pscustomobject]@{phase=$Phase; debug=$Level; seconds=$seconds})
}

try {
    Invoke-ProfileBuild "2" "warmup"
    Invoke-ProfileBuild "1" "warmup"
    foreach ($level in @("2", "1", "1", "2")) {
        Invoke-ProfileBuild $level "measured"
    }
    foreach ($level in @("1", "2")) {
        $target = if ($level -eq "1") { Join-Path $PWD "target" } else { $probeTarget }
        $size = (Get-ChildItem $target -Recurse -File | Measure-Object Length -Sum).Sum
        Write-Output "CI_DEBUG_PROBE debug=$level target_bytes=$size"
    }
    $results | ConvertTo-Json | Out-File -FilePath $env:GITHUB_STEP_SUMMARY -Append
} finally {
    Remove-Item $probeTarget -Recurse -Force -ErrorAction SilentlyContinue
}
