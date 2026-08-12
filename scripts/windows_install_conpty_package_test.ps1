param(
    [Parameter(Mandatory = $true)]
    [string]$ArchivePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$installerPath = (Resolve-Path -LiteralPath "$PSScriptRoot\..\website\install.ps1").Path
$parseErrors = $null
$tokens = $null
$installerAst = [System.Management.Automation.Language.Parser]::ParseFile(
    $installerPath,
    [ref]$tokens,
    [ref]$parseErrors
)
if ($parseErrors.Count -ne 0) {
    throw ($parseErrors | Out-String)
}
foreach ($functionName in @("Prepend-PathEntry", "Update-PathRegistryEntry")) {
    $definition = $installerAst.FindAll(
        {
            param($node)
            $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq $functionName
        },
        $true
    ) | Select-Object -First 1
    if ($null -eq $definition) {
        throw "installer is missing function $functionName"
    }
    Invoke-Expression $definition.Extent.Text
}

$pathTestVariable = "HERDR_INSTALLER_PATH_TEST"
$oldPathTestVariable = [Environment]::GetEnvironmentVariable($pathTestVariable, "Process")
$testRegistryPath = "Software\HerdrInstallerTests-$([Guid]::NewGuid().ToString('N'))"
$testEnvironmentKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($testRegistryPath)
if ($null -eq $testEnvironmentKey) {
    throw "unable to create temporary installer test registry key"
}
try {
    [Environment]::SetEnvironmentVariable($pathTestVariable, "C:\expanded", "Process")
    $testEnvironmentKey.SetValue(
        "Path",
        "%$pathTestVariable%\bin;C:\existing",
        [Microsoft.Win32.RegistryValueKind]::ExpandString
    )
    $pathChanged = Update-PathRegistryEntry -EnvironmentKey $testEnvironmentKey -Entry "C:\Herdr\bin"
    if (-not $pathChanged) {
        throw "installer PATH update reported no change"
    }
    if (Update-PathRegistryEntry -EnvironmentKey $testEnvironmentKey -Entry "C:\Herdr\bin") {
        throw "installer PATH update was not idempotent"
    }

    $options = [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    $rawPath = $testEnvironmentKey.GetValue("Path", $null, $options)
    $expectedPath = "C:\Herdr\bin;%$pathTestVariable%\bin;C:\existing"
    if ($rawPath -cne $expectedPath) {
        throw "installer changed raw PATH: expected '$expectedPath', got '$rawPath'"
    }
    if ($testEnvironmentKey.GetValueKind("Path") -ne [Microsoft.Win32.RegistryValueKind]::ExpandString) {
        throw "installer changed the PATH registry value kind"
    }
} finally {
    $testEnvironmentKey.Dispose()
    [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($testRegistryPath, $false)
    [Environment]::SetEnvironmentVariable($pathTestVariable, $oldPathTestVariable, "Process")
}

$archive = (Resolve-Path -LiteralPath $ArchivePath).Path
$root = Join-Path $env:RUNNER_TEMP ("herdr-installer-test-" + [Guid]::NewGuid().ToString("N"))
$webRoot = Join-Path $root "web"
$herdrHome = Join-Path $root "home"
$installDir = Join-Path $root "bin"
New-Item -ItemType Directory -Force -Path $webRoot | Out-Null
Copy-Item -LiteralPath $archive -Destination (Join-Path $webRoot "herdr-windows-x86_64.zip")
$hash = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
$listener.Stop()
$manifest = @{
    channel = "preview"
    base_version = "0.0.0"
    build_id = "installer-test"
    assets = @{
        "windows-x86_64" = @{
            url = "http://127.0.0.1:$port/herdr-windows-x86_64.zip"
            sha256 = $hash
            format = "zip"
        }
    }
} | ConvertTo-Json -Depth 5
$manifestPath = Join-Path $webRoot "preview.json"
$manifest | Out-File -LiteralPath $manifestPath -Encoding utf8

$server = $null
$oldHerdrHome = $env:HERDR_HOME
try {
    $server = Start-Process python -ArgumentList @("-m", "http.server", "$port", "--bind", "127.0.0.1", "--directory", $webRoot) -PassThru -WindowStyle Hidden
    $env:HERDR_HOME = Join-Path $root "unused\..\home"
    $manifestUrl = "http://127.0.0.1:$port/preview.json"
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        try {
            Invoke-WebRequest -Uri $manifestUrl -UseBasicParsing | Out-Null
            break
        } catch {
            if ($attempt -eq 19) { throw }
            Start-Sleep -Milliseconds 100
        }
    }

    & "$PSScriptRoot\..\website\install.ps1" `
        -ManifestUrl $manifestUrl `
        -InstallDir $installDir `
        -ExpectedBuildId "installer-test"

    $required = @(
        "herdr.exe",
        "conpty\herdr-conpty.json",
        "conpty\conpty.dll",
        "conpty\x64\OpenConsole.exe",
        "conpty\arm64\OpenConsole.exe",
        "THIRD-PARTY-NOTICES\Microsoft.Windows.Console.ConPTY-LICENSE.txt",
        "THIRD-PARTY-NOTICES\Microsoft.Windows.Console.ConPTY-NOTICE.md"
    )
    foreach ($relative in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $installDir $relative) -PathType Leaf)) {
            throw "installer did not activate required file $relative"
        }
    }

    $releasesDir = Join-Path $herdrHome "packages\standalone\releases"
    $releaseDir = Get-ChildItem -LiteralPath $releasesDir -Directory |
        Where-Object { -not $_.Name.StartsWith(".staging.") } |
        Select-Object -First 1
    if ($null -eq $releaseDir) {
        throw "installer did not create a versioned release"
    }
    Remove-Item -LiteralPath (Join-Path $releaseDir.FullName "conpty\conpty.dll") -Force

    $badManifest = $manifest | ConvertFrom-Json
    $badManifest.assets."windows-x86_64".url = "http://127.0.0.1:$port/missing.zip"
    $badManifest | ConvertTo-Json -Depth 5 | Out-File -LiteralPath $manifestPath -Encoding utf8
    $downloadFailed = $false
    try {
        & "$PSScriptRoot\..\website\install.ps1" `
            -ManifestUrl $manifestUrl `
            -InstallDir $installDir `
            -ExpectedBuildId "installer-test"
    } catch {
        if ($_.Exception.Message -notmatch "(?i)(404|not found)") {
            throw
        }
        $downloadFailed = $true
    }
    if (-not $downloadFailed) {
        throw "installer repair unexpectedly accepted a missing archive"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $releaseDir.FullName "herdr.exe") -PathType Leaf)) {
        throw "failed repair removed the existing release"
    }

    $manifest | Out-File -LiteralPath $manifestPath -Encoding utf8
    $stagedConpty = Join-Path $releasesDir ".staging.$($releaseDir.Name).$PID\conpty\conpty.dll"
    $lockState = @{ Handle = $null }
    $lockStagedFile = {
        if ($null -eq $lockState.Handle) {
            $lockState.Handle = [System.IO.File]::Open(
                $stagedConpty,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::Read
            )
        }
    }.GetNewClosure()
    $swapBreakpoint = Set-PSBreakpoint -Script $installerPath -Variable "backupDir" -Mode Write -Action $lockStagedFile
    try {
        $swapFailed = $false
        try {
            & "$PSScriptRoot\..\website\install.ps1" `
                -ManifestUrl $manifestUrl `
                -InstallDir $installDir `
                -ExpectedBuildId "installer-test"
        } catch {
            $swapFailed = $true
        }
        if ($null -eq $lockState.Handle) {
            throw "installer did not acquire the staged file handle before the swap"
        }
        if (-not $swapFailed) {
            throw "installer unexpectedly activated a release with a locked staged file"
        }
        if (-not (Test-Path -LiteralPath (Join-Path $releaseDir.FullName "herdr.exe") -PathType Leaf)) {
            throw "failed activation did not restore the prior release"
        }
        if (@(Get-ChildItem -LiteralPath $releasesDir -Force -Directory -Filter ".backup.$($releaseDir.Name).*").Count -ne 0) {
            throw "failed activation stranded a release backup"
        }
        foreach ($junction in @($installDir, (Join-Path $herdrHome "packages\standalone\current"))) {
            if (-not (Test-Path -LiteralPath (Join-Path $junction "herdr.exe") -PathType Leaf)) {
                throw "failed activation left an invalid installer junction at $junction"
            }
        }
    } finally {
        Remove-PSBreakpoint -Breakpoint $swapBreakpoint
        if ($null -ne $lockState.Handle) {
            $lockState.Handle.Dispose()
        }
    }

    & "$PSScriptRoot\..\website\install.ps1" `
        -ManifestUrl $manifestUrl `
        -InstallDir $installDir `
        -ExpectedBuildId "installer-test"
    if (-not (Test-Path -LiteralPath (Join-Path $installDir "conpty\conpty.dll") -PathType Leaf)) {
        throw "installer did not repair an incomplete release"
    }

    $x64HostDir = Join-Path $releaseDir.FullName "conpty\x64"
    $junctionTarget = Join-Path $root "junction-target"
    Move-Item -LiteralPath $x64HostDir -Destination $junctionTarget
    New-Item -ItemType Junction -Path $x64HostDir -Target $junctionTarget | Out-Null
    & "$PSScriptRoot\..\website\install.ps1" `
        -ManifestUrl $manifestUrl `
        -InstallDir $installDir `
        -ExpectedBuildId "installer-test"
    $repairedHostDir = Get-Item -LiteralPath (Join-Path $installDir "conpty\x64") -Force
    if ($repairedHostDir.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "installer accepted a reparse-point ConPTY directory"
    }

    $rejected = $false
    try {
        & "$PSScriptRoot\..\website\install.ps1" `
            -ManifestUrl $manifestUrl `
            -InstallDir $installDir `
            -ExpectedBuildId "different-build"
    } catch {
        if ($_.Exception.Message -notlike "Preview manifest changed while updating.*") {
            throw
        }
        $rejected = $true
    }
    if (-not $rejected) {
        throw "installer accepted a manifest that did not match the updater-selected build"
    }
} finally {
    $env:HERDR_HOME = $oldHerdrHome
    if ($null -ne $server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
