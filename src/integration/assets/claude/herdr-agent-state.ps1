# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=11

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

if (-not [string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }
if ($payload.hook_event_name -eq "SubagentStop") { exit 0 }

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$transcriptPath = $payload.transcript_path
if ([string]::IsNullOrWhiteSpace($transcriptPath) -and $sessionId -notmatch '[\\/]') {
    $projectCwd = $payload.cwd
    if ([string]::IsNullOrWhiteSpace($projectCwd)) { $projectCwd = (Get-Location).Path }
    $configRoot = $env:CLAUDE_CONFIG_DIR
    if ([string]::IsNullOrWhiteSpace($configRoot)) { $configRoot = Join-Path $HOME ".claude" }
    $projectKey = ([IO.Path]::GetFullPath($projectCwd) -replace '[\\/]', '-')
    $transcriptPath = Join-Path (Join-Path (Join-Path ([IO.Path]::GetFullPath($configRoot)) "projects") $projectKey) "$sessionId.jsonl"
}

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
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
    if (-not [string]::IsNullOrWhiteSpace($transcriptPath)) {
        $args += @("--agent-session-path", "$transcriptPath")
    }
    if ($payload.hook_event_name -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $args += @("--session-start-source", "$($payload.source)")
    }
    & herdr @args 2>$null | Out-Null
} catch {
}
