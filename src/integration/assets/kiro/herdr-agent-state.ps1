# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=kiro
# HERDR_INTEGRATION_VERSION=1

if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_SOCKET_PATH)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}
if ($null -eq $payload) { exit 0 }

function Test-PositiveInteger([object]$Value) {
    return ($Value -is [int] -or $Value -is [long]) -and $Value -gt 0
}

function Test-PositiveSafeInteger([object]$Value) {
    return (Test-PositiveInteger $Value) -and $Value -le 9007199254740991
}

if ($payload.hook_event_name -ne "SessionChange") { exit 0 }
if ($payload.session_id -isnot [string] -or [string]::IsNullOrWhiteSpace($payload.session_id)) { exit 0 }
if ($payload.cwd -isnot [string] -or [string]::IsNullOrWhiteSpace($payload.cwd)) { exit 0 }
if ($payload.session_location -notin @("local", "remote")) { exit 0 }
if (-not (Test-PositiveInteger $payload.client_pid)) { exit 0 }
if (-not (Test-PositiveSafeInteger $payload.transition_seq)) { exit 0 }

try {
    $null = Get-Process -Id ([int]$payload.client_pid) -ErrorAction Stop
} catch {
    exit 0
}

$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
if (-not [string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH) -and -not (Test-Path -LiteralPath $herdr -PathType Leaf)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH) -and $null -eq (Get-Command $herdr -ErrorAction SilentlyContinue)) { exit 0 }

$commandArgs = @(
    "pane",
    $(if ($payload.session_location -eq "local") { "report-agent-session" } else { "release-agent" }),
    $env:HERDR_PANE_ID,
    "--source",
    "herdr:kiro-v3",
    "--agent",
    "kiro",
    "--seq",
    [string]$payload.transition_seq
)
if ($payload.session_location -eq "local") {
    $commandArgs += @(
        "--agent-session-id",
        [string]$payload.session_id,
        "--session-start-source",
        "select"
    )
}

$job = $null
try {
    $job = Start-Job -ScriptBlock {
        param($Executable, [object[]]$Arguments)
        & $Executable @Arguments *> $null
    } -ArgumentList $herdr, (,$commandArgs)
    if ($null -eq (Wait-Job -Job $job -Timeout 1)) {
        Stop-Job -Job $job
    }
} catch {
} finally {
    if ($null -ne $job) {
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
}
