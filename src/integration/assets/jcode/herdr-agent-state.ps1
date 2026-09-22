# installed by herdr
# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=jcode
# HERDR_INTEGRATION_VERSION=1

if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_SOCKET_PATH)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:JCODE_HOOK_SESSION_ID)) { exit 0 }

$sessionStartSource = switch ($env:JCODE_HOOK_SOURCE) {
    "create" { "startup" }
    "attach" { "startup" }
    "resume" { "resume" }
    default { $null }
}

$seq = [DateTime]::UtcNow.Ticks
$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
$herdrArgs = @(
    "pane", "report-agent-session", $env:HERDR_PANE_ID,
    "--source", "herdr:jcode",
    "--agent", "jcode",
    "--seq", "$seq",
    "--agent-session-id", $env:JCODE_HOOK_SESSION_ID
)
if (-not [string]::IsNullOrWhiteSpace($sessionStartSource)) {
    $herdrArgs += @("--session-start-source", "$sessionStartSource")
}
try {
    & $herdr @herdrArgs 2>$null | Out-Null
} catch {
}

# Session reporting must never prevent Jcode from starting.
exit 0
