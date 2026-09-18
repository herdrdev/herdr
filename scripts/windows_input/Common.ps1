# Shared test helpers. Dot-sourcing does not touch the desktop or console.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-GauntletJson($Path, $Value) {
    $temporary = "$Path.$([guid]::NewGuid().ToString('N')).tmp"
    [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 30), [Text.UTF8Encoding]::new($false))
    for ($attempt = 0; ; $attempt++) {
        try { [IO.File]::Move($temporary, $Path, $true); return }
        catch [IO.IOException], [UnauthorizedAccessException] {
            if ($attempt -ge 99) { throw }
            Start-Sleep -Milliseconds 10
        }
    }
}

function Read-GauntletJson($Path) {
    if (Test-Path -LiteralPath $Path) {
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
        try {
            $reader = [IO.StreamReader]::new($stream)
            try { return $reader.ReadToEnd() | ConvertFrom-Json -AsHashtable }
            finally { $reader.Dispose() }
        } finally { $stream.Dispose() }
    }
    return $null
}

function Quote-GauntletPS([string] $Text) { return "'" + $Text.Replace("'", "''") + "'" }

function New-GauntletProcess($Exe, $Arguments, $Plan, [switch] $Capture) {
    $info = [Diagnostics.ProcessStartInfo]::new($Exe)
    $info.UseShellExecute = $false
    foreach ($argument in $Arguments) { $info.ArgumentList.Add([string]$argument) }
    foreach ($name in @($info.Environment.Keys)) {
        if ($name -like 'HERDR_*' -or $name -in @('SSH_CONNECTION', 'SSH_TTY')) { $null = $info.Environment.Remove($name) }
    }
    if ($null -ne $Plan) {
        $info.Environment['HERDR_CONFIG_PATH'] = $Plan.config
        $info.Environment['XDG_CONFIG_HOME'] = $Plan.config_home
        $info.Environment['HERDR_SESSION'] = $Plan.session
        if ($Plan.profile -ne 'default') { $info.Environment['HERDR_WINDOWS_INPUT_PROBE'] = $Plan.profile }
        if ($Plan.path -in @('herdr', 'herdr-remote')) { $info.Environment['HERDR_WINDOWS_INPUT_TRACE_FILE'] = $Plan.input_trace }
        if ($Plan.path -eq 'herdr-remote') {
            $info.Environment['HERDR_REMOTE_KEYBINDINGS'] = 'local'
        }
    }
    $info.RedirectStandardOutput = $Capture.IsPresent
    $info.RedirectStandardError = $Capture.IsPresent
    $info.CreateNoWindow = $Capture.IsPresent
    return [Diagnostics.Process]::Start($info)
}

function Invoke-GauntletProcess($Exe, $Arguments, $Plan = $null, [int] $Timeout = 30) {
    $process = New-GauntletProcess $Exe $Arguments $Plan -Capture
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $timer = [Diagnostics.Stopwatch]::StartNew()
        if (-not $process.WaitForExit($Timeout * 1000)) {
            $process.Kill($true)
            throw "Command timed out: $Exe $($Arguments -join ' ')"
        }
        $remaining = [Math]::Max(0, $Timeout * 1000 - [int]$timer.ElapsedMilliseconds)
        if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($stdout, $stderr), $remaining)) {
            throw "Output streams stayed open after exit: $Exe $($Arguments -join ' ')"
        }
        if ($process.ExitCode -ne 0) { throw "Command failed ($($process.ExitCode)): $Exe`n$($stderr.GetAwaiter().GetResult())" }
        return $stdout.GetAwaiter().GetResult()
    } finally { $process.Dispose() }
}

function Update-GauntletLease($Root) {
    [IO.File]::WriteAllText((Join-Path $Root 'lease'), [DateTime]::UtcNow.ToString('O'))
}

function Test-GauntletLease($Root) {
    $lease = Get-Item -LiteralPath (Join-Path $Root 'lease') -ErrorAction SilentlyContinue
    return $null -ne $lease -and ([DateTime]::UtcNow - $lease.LastWriteTimeUtc).TotalSeconds -lt 90
}

function Wait-GauntletJson($Path, $Root, $Predicate, [int] $Timeout = 20) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Timeout)
    do {
        Update-GauntletLease $Root
        $value = Read-GauntletJson $Path
        if ($null -ne $value -and (& $Predicate $value)) { return $value }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for fresh acknowledgement: $Path"
}
