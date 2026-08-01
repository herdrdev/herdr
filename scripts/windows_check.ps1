param(
    [ValidateSet("lint", "check")]
    [string]$Mode = "check"
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments)

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $Command $($Arguments -join ' ')"
    }
}

function Invoke-CargoWithZigCacheRecovery {
    param([string[]]$Arguments)

    & cargo @Arguments
    if ($LASTEXITCODE -eq 0) {
        return
    }

    Write-Warning "cargo compile failed; clearing Zig build caches and retrying once"
    Remove-Item -Recurse -Force .zig-cache -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force vendor/libghostty-vt/.zig-cache -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force vendor/libghostty-vt/zig-out -ErrorAction SilentlyContinue
    Invoke-Checked cargo $Arguments
}

function Invoke-CargoTestFilter {
    param(
        [Parameter(Mandatory)]
        [string]$Filter,
        [switch]$Exact
    )

    $commonArguments = @(
        "test",
        "--locked",
        "--target",
        "x86_64-pc-windows-msvc",
        "--bin",
        "herdr",
        $Filter
    )
    $harnessArguments = @("--list")
    if ($Exact) {
        $harnessArguments += "--exact"
    }

    $listArguments = $commonArguments + @("--") + $harnessArguments
    $listOutput = @(& cargo @listArguments)
    if ($LASTEXITCODE -ne 0) {
        throw "could not enumerate tests for filter '$Filter': $($listOutput -join [Environment]::NewLine)"
    }

    $testNames = @(
        foreach ($line in $listOutput) {
            $match = [regex]::Match([string]$line, '^\s*(\S+): test\s*$')
            if ($match.Success) {
                $match.Groups[1].Value
            }
        }
    )
    if ($testNames.Count -eq 0) {
        throw "test filter '$Filter' selected zero tests"
    }

    Write-Host "Running $($testNames.Count) test(s) for '$Filter'"
    $runArguments = $commonArguments
    if ($Exact) {
        $runArguments += @("--", "--exact")
    }
    Invoke-Checked cargo $runArguments
}

Invoke-Checked rustup @("target", "add", "x86_64-pc-windows-msvc")
Invoke-Checked cargo @("fmt", "--check")
Invoke-CargoWithZigCacheRecovery @(
    "clippy",
    "--bin",
    "herdr",
    "--locked",
    "--target",
    "x86_64-pc-windows-msvc",
    "--",
    "-D",
    "warnings"
)

if ($Mode -eq "lint") {
    return
}

Invoke-CargoTestFilter "windows_"
Invoke-CargoTestFilter "server::client_transport::tests"
Invoke-CargoTestFilter "app::tests::native_repeats_and_releases_follow_the_pressed_pane" -Exact
Invoke-Checked cargo @("build", "--locked", "--target", "x86_64-pc-windows-msvc")
