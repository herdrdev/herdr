# installed by shepherd
# managed by shepherd; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# SHEPHERD_INTEGRATION_ID=antigravity_cli
# SHEPHERD_INTEGRATION_VERSION=4

# Session-only: this hook reports the Antigravity conversation so Shepherd can
# resume the pane. Lifecycle state comes from Shepherd's screen detection.

param([string]$Action = "")

# Antigravity CLI expects a JSON object on stdout and this hook never injects
# anything, so every exit path emits an empty object.
function Exit-Hook {
    Write-Output "{}"
    exit 0
}

if ($Action -ne "session") { Exit-Hook }
if ($env:SHEPHERD_ENV -ne "1") { Exit-Hook }
if ([string]::IsNullOrWhiteSpace($env:SHEPHERD_PANE_ID)) { Exit-Hook }

# Read hook input as raw bytes so UTF-8 payloads decode correctly even when the
# console input code page is not UTF-8 (for example CP949 on Korean Windows).
$stdinReader = New-Object System.IO.StreamReader([Console]::OpenStandardInput(), [System.Text.Encoding]::UTF8)
$inputText = $stdinReader.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    Exit-Hook
}

if ($null -eq $payload) { Exit-Hook }

$conversationId = if ($payload.conversationId -is [string]) { $payload.conversationId } else { $null }
if ([string]::IsNullOrWhiteSpace($conversationId)) { Exit-Hook }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$shepherd = if ([string]::IsNullOrWhiteSpace($env:SHEPHERD_BIN_PATH)) { "shepherd" } else { $env:SHEPHERD_BIN_PATH }
try {
    $sessionArgs = @(
        "pane",
        "report-agent-session",
        $env:SHEPHERD_PANE_ID,
        "--source",
        "shepherd:antigravity_cli",
        "--agent",
        "agy",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$conversationId"
    )
    if ($payload.transcriptPath -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcriptPath)) {
        $sessionArgs += @("--agent-session-path", "$($payload.transcriptPath)")
    }
    & $shepherd @sessionArgs 2>$null | Out-Null
} catch {
}

Exit-Hook
