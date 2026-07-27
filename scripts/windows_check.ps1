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

Invoke-Checked cargo @(
    "test",
    "--locked",
    "--target",
    "x86_64-pc-windows-msvc",
    "--bin",
    "herdr",
    "windows_"
)
Invoke-Checked cargo @(
    "test",
    "--locked",
    "--target",
    "x86_64-pc-windows-msvc",
    "--bin",
    "herdr",
    "server::client_transport::tests"
)
Invoke-Checked cargo @("build", "--locked", "--target", "x86_64-pc-windows-msvc")
