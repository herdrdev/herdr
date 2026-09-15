# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=muse
# HERDR_INTEGRATION_VERSION=1

param([string]$Action = "")

if (@("session", "working", "blocked", "idle") -notcontains $Action) { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$sessionId = if ($null -ne $payload -and -not [string]::IsNullOrWhiteSpace($payload.session_id)) { $payload.session_id } else { $null }
$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }

try {
    if ($Action -eq "session") {
        # Subagent SessionStart hooks carry agent_id; only the top-level
        # session may claim the pane (like the claude hook).
        if ($null -ne $payload -and -not [string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }
        if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }
        & $herdr pane report-agent-session $env:HERDR_PANE_ID --source herdr:muse --agent muse --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
    } else {
        if ([string]::IsNullOrWhiteSpace($sessionId)) {
            & $herdr pane report-agent $env:HERDR_PANE_ID --source herdr:muse --agent muse --state $Action --seq $seq 2>$null | Out-Null
        } else {
            & $herdr pane report-agent $env:HERDR_PANE_ID --source herdr:muse --agent muse --state $Action --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
        }
    }
} catch {
}
