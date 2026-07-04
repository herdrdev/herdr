# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=antigravity_cli
# HERDR_INTEGRATION_VERSION=1

param([string]$Action = "")

if ($Action -ne "working" -and $Action -ne "idle" -and $Action -ne "release") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

$conversationId = $null
$transcriptPath = $null
if ($null -ne $payload) {
    if ($payload.conversationId -is [string]) {
        $conversationId = $payload.conversationId
    }
    if ($payload.transcriptPath -is [string]) {
        $transcriptPath = $payload.transcriptPath
    }
}

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()

# Report session if we have a conversationId
if (-not [string]::IsNullOrWhiteSpace($conversationId)) {
    try {
        $sessionArgs = @(
            "pane",
            "report-agent-session",
            $env:HERDR_PANE_ID,
            "--source",
            "herdr:antigravity_cli",
            "--agent",
            "antigravity-cli",
            "--seq",
            "$seq",
            "--agent-session-id",
            "$conversationId"
        )
        if (-not [string]::IsNullOrWhiteSpace($transcriptPath)) {
            $sessionArgs += @("--agent-session-path", "$transcriptPath")
        }
        & herdr @sessionArgs 2>$null | Out-Null
    } catch {}
}

# Report status or release
try {
    if ($Action -eq "release") {
        $args = @(
            "pane",
            "release-agent",
            $env:HERDR_PANE_ID,
            "--source",
            "herdr:antigravity_cli",
            "--agent",
            "antigravity-cli",
            "--seq",
            "$seq"
        )
        & herdr @args 2>$null | Out-Null
    } else {
        $args = @(
            "pane",
            "report-agent",
            $env:HERDR_PANE_ID,
            "--source",
            "herdr:antigravity_cli",
            "--agent",
            "antigravity-cli",
            "--state",
            $Action,
            "--seq",
            "$seq"
        )
        if (-not [string]::IsNullOrWhiteSpace($conversationId)) {
            $args += @("--agent-session-id", "$conversationId")
        }
        & herdr @args 2>$null | Out-Null
    }
} catch {}
