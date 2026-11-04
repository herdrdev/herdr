# installed by shepherd
# managed by shepherd; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# SHEPHERD_INTEGRATION_ID=claude
# SHEPHERD_INTEGRATION_VERSION=11

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:SHEPHERD_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:SHEPHERD_PANE_ID)) { exit 0 }

# Read hook input as raw bytes so UTF-8 payloads decode correctly even when the
# console input code page is not UTF-8 (for example CP949 on Korean Windows).
$stdinReader = New-Object System.IO.StreamReader([Console]::OpenStandardInput(), [System.Text.Encoding]::UTF8)
$inputText = $stdinReader.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

$propertyNames = @($payload.PSObject.Properties.Name)
if ((Test-Path Env:CURSOR_VERSION) -or $propertyNames -ccontains "cursor_version") { exit 0 }
if (-not ($propertyNames -ccontains "hook_event_name") -or $payload.hook_event_name -isnot [string] -or $payload.hook_event_name -cne "SessionStart") { exit 0 }
if (-not [string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$shepherd = if ([string]::IsNullOrWhiteSpace($env:SHEPHERD_BIN_PATH)) { "shepherd" } else { $env:SHEPHERD_BIN_PATH }
try {
    $args = @(
        "pane",
        "report-agent-session",
        $env:SHEPHERD_PANE_ID,
        "--source",
        "shepherd:claude",
        "--agent",
        "claude",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$sessionId"
    )
    if ($payload.transcript_path -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcript_path)) {
        $args += @("--agent-session-path", "$($payload.transcript_path)")
    }
    if ($payload.hook_event_name -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $args += @("--session-start-source", "$($payload.source)")
    }
    & $shepherd @args 2>$null | Out-Null
} catch {
}
