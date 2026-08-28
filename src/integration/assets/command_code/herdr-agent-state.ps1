# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=command-code
# HERDR_INTEGRATION_VERSION=1

param(
    [Parameter(Position=0)]
    [string]$Action = "session"
)

if ($Action -ne "session") {
    exit 0
}

if ($env:HERDR_ENV -ne "1") { exit 0 }
if (-not $env:HERDR_SOCKET_PATH) { exit 0 }
if (-not $env:HERDR_PANE_ID) { exit 0 }

$hookInput = [Console]::In.ReadToEnd() | ConvertFrom-Json -ErrorAction SilentlyContinue
if (-not $hookInput) { $hookInput = @{} }

$hookEventName = $hookInput.hook_event_name
if ($hookEventName -and $hookEventName -ne "SessionStart") { exit 0 }

$sessionId = $hookInput.session_id
if (-not $sessionId) { exit 0 }

$transcriptPath = $hookInput.transcript_path

$source = "herdr:command_code"
$requestId = "$source`:$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()):$(Get-Random -Maximum 999999)"
$reportSeq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() * 1000000

$reportArgs = @(
    "pane",
    "report-agent-session",
    $env:HERDR_PANE_ID,
    "--source", $source,
    "--agent", "command-code",
    "--seq", $reportSeq,
    "--agent-session-id", $sessionId
)
if ($transcriptPath) {
    $reportArgs += "--agent-session-path"
    $reportArgs += $transcriptPath
}

& $env:HERDR_BIN_PATH @reportArgs 2>$null
