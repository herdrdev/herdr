param(
    [Parameter(Mandatory = $true)]
    [string]$HerdrExe,

    [Parameter(Mandatory = $true)]
    [string]$PackagePath,

    [Parameter(Mandatory = $true)]
    [string]$StageDir,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-NativeChecked {
    param(
        [string]$Command,
        [string[]]$Arguments
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

$packager = Join-Path $PSScriptRoot "package_windows_conpty.py"
Invoke-NativeChecked python @(
    $packager,
    "stage",
    "--package", $PackagePath,
    "--herdr-exe", $HerdrExe,
    "--output-dir", $StageDir
)
Invoke-NativeChecked dotnet @("nuget", "verify", "--all", $PackagePath)

foreach ($relative in @("conpty\conpty.dll", "conpty\x64\OpenConsole.exe", "conpty\arm64\OpenConsole.exe")) {
    $signature = Get-AuthenticodeSignature (Join-Path $StageDir $relative)
    $subject = if ($null -eq $signature.SignerCertificate) { "" } else { $signature.SignerCertificate.Subject }
    if ($signature.Status -ne "Valid" -or $subject -notlike "*Microsoft Corporation*") {
        throw "Invalid Microsoft signature for $relative`: $($signature.Status) $subject"
    }
}

Invoke-NativeChecked python @(
    $packager,
    "archive",
    "--stage-dir", $StageDir,
    "--output", $OutputPath
)
