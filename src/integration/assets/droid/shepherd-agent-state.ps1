# installed by shepherd
# managed by shepherd; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# SHEPHERD_INTEGRATION_ID=droid
# SHEPHERD_INTEGRATION_VERSION=4

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
    $payload = $null
}

if ($null -eq $payload -or [string]::IsNullOrWhiteSpace($payload.session_id)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$shepherd = if ([string]::IsNullOrWhiteSpace($env:SHEPHERD_BIN_PATH)) { "shepherd" } else { $env:SHEPHERD_BIN_PATH }
try {
    & $shepherd pane report-agent-session $env:SHEPHERD_PANE_ID --source shepherd:droid --agent droid --agent-session-id $payload.session_id --seq $seq 2>$null | Out-Null
} catch {
}
