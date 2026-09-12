# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=devin
# HERDR_INTEGRATION_VERSION=2

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

$sessionId = $null
if ($null -ne $payload) {
    if ($payload.session_id -is [string]) { $sessionId = $payload.session_id }
    elseif ($payload.sessionId -is [string]) { $sessionId = $payload.sessionId }
}

$event = if ($null -ne $payload -and $payload.hook_event_name -is [string]) { $payload.hook_event_name } else { "" }
$allowFallback = $event -ne "UserPromptSubmit" -and -not ($event -eq "SessionStart" -and $payload.source -eq "startup")
if ([string]::IsNullOrWhiteSpace($sessionId) -and $allowFallback) {
    $projectDir = if ([string]::IsNullOrWhiteSpace($env:DEVIN_PROJECT_DIR)) { (Get-Location).Path } else { $env:DEVIN_PROJECT_DIR }
    try {
        $sessions = & devin list --format json 2>$null | ConvertFrom-Json
        $normalizedProject = [IO.Path]::GetFullPath($projectDir).TrimEnd('\')
        foreach ($session in @($sessions)) {
            if ($session.id -isnot [string] -or $session.working_directory -isnot [string]) { continue }
            $workingDirectory = [IO.Path]::GetFullPath($session.working_directory).TrimEnd('\')
            if ($workingDirectory -ieq $normalizedProject) {
                $sessionId = $session.id
                break
            }
        }
    } catch {
    }
}
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTime]::UtcNow.Ticks
$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
try {
    & $herdr pane report-agent-session $env:HERDR_PANE_ID --source herdr:devin --agent devin --seq $seq --agent-session-id $sessionId 2>$null | Out-Null
} catch {
}
