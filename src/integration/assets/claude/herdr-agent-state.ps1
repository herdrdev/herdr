# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=11

param([string]$Action = "")

if ($Action -ne "session" -and $Action -ne "title" -and $Action -ne "session-name" -and $Action -ne "notification") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
if ($Action -eq "title" -or $Action -eq "session-name") {
    $herdrBin = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
    if ($Action -eq "title") {
        try {
            $inputText | & $herdrBin agent turn-title --provider claude 2>$null | Out-Null
        } catch {
        }
    }
    # Claude renames a session outside turn boundaries, so publish the current
    # name on every event this hook receives, not only at turn start.
    try {
        $inputText | & $herdrBin agent session-name --provider claude 2>$null | Out-Null
    } catch {
    }
    exit 0
}
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

$propertyNames = @($payload.PSObject.Properties.Name)
if ((Test-Path Env:CURSOR_VERSION) -or $propertyNames -ccontains "cursor_version") { exit 0 }
if (-not ($propertyNames -ccontains "hook_event_name") -or $payload.hook_event_name -isnot [string] -or ($payload.hook_event_name -cne "SessionStart" -and $payload.hook_event_name -cne "Notification")) { exit 0 }
if (-not [string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
if ($payload.hook_event_name -eq "SessionStart") {
    try {
        $stateRoot = if ([string]::IsNullOrWhiteSpace($env:XDG_STATE_HOME)) { Join-Path $HOME ".local/state" } else { $env:XDG_STATE_HOME }
        $mirrorDir = Join-Path $stateRoot "herdr/agent-status"
        New-Item -ItemType Directory -Force -Path $mirrorDir | Out-Null
        $mirrorPath = Join-Path $mirrorDir ($env:HERDR_PANE_ID.Replace(":", "_") + ".json")
        $prior = if (Test-Path $mirrorPath) { Get-Content -Raw $mirrorPath | ConvertFrom-Json } else { $null }
        if ($prior.session_id -ne $sessionId) {
            $staging = Join-Path $mirrorDir ([System.Guid]::NewGuid().ToString() + ".tmp")
            @{ v = 2; agent = "claude"; session_id = $sessionId; blocking = 0; agents = 0; gates = @(); items = @(); seq = $seq } | ConvertTo-Json -Compress | Set-Content -Path $staging
            Move-Item -Force -Path $staging -Destination $mirrorPath
        }
    } catch { }
}
$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
try {
    $args = @(
        "pane",
        "report-agent-session",
        $env:HERDR_PANE_ID,
        "--source",
        "herdr:claude",
        "--agent",
        "claude",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$sessionId"
    )
    if ($payload.hook_event_name -eq "Notification") {
        if ($payload.notification_type -notin @("permission_prompt", "idle_prompt")) { exit 0 }
        $args[1] = "report-agent"
        $args += @("--state", "blocked", "--message", "$($payload.notification_type)")
    }
    if ($payload.transcript_path -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcript_path)) {
        $args += @("--agent-session-path", "$($payload.transcript_path)")
    }
    if ($payload.hook_event_name -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $args += @("--session-start-source", "$($payload.source)")
    }
    & $herdr @args 2>$null | Out-Null
} catch {
}
