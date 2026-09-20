#Requires -Version 7.0
<# Local interactive qualification, deliberately excluded from normal CI.
   F12 aborts before the next injected gesture. Use an isolated unlocked desktop.
   SendInput has an unavoidable focus race: do not use this while doing other work.
#>
[CmdletBinding()]
param(
    [string] $ExePath,
    [string] $StablePath,
    [string] $PreviewPath,
    [ValidateSet('default', 'win32', 'vt')][string] $Profile = 'default',
    [ValidateSet('native', 'legacy', 'mok2', 'kitty')][string[]] $Modes = @('native', 'legacy', 'mok2', 'kitty'),
    [ValidateSet('stable', 'preview')][string[]] $Channels = @('stable', 'preview'),
    [ValidateSet('direct', 'herdr', 'herdr-remote')][string[]] $Paths = @('direct', 'herdr'),
    [string[]] $Cases,
    [int[]] $Widths = @(80, 119, 120, 121, 132, 160, 240),
    [int[]] $Heights = @(24, 50),
    [string] $OutputDirectory,
    [switch] $AllowInputInjection,
    [switch] $Manual,
    [switch] $MatrixOnly
)
. "$PSScriptRoot/windows_input/Common.ps1"
$reportScript = Join-Path $PSScriptRoot 'windows_input/report.py'
$python = (Get-Command python -ErrorAction Stop).Source
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $PWD ".local/windows-input/$([guid]::NewGuid().ToString('N'))" }
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Output directory must be new, to prevent stale evidence or overwrites' }
$root = [IO.Directory]::CreateDirectory($OutputDirectory).FullName
$null = Invoke-GauntletProcess $python @($reportScript, 'matrix', '--output', (Join-Path $root 'matrix.json'))
$matrix = Read-GauntletJson (Join-Path $root 'matrix.json')
if ($MatrixOnly) { Write-Host "Matrix: $root/matrix.json"; exit 0 }
$caseSelectionProvided = $PSBoundParameters.ContainsKey('Cases')
$Cases = @($Cases | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
$knownCases = @($matrix.cases | ForEach-Object id)
if (($caseSelectionProvided -and -not @($Cases).Count) -or @($Cases | Where-Object { $_ -notin $knownCases }).Count) { throw 'Unknown case selection' }
$selectedCases = if (@($Cases).Count) { @($matrix.cases | Where-Object id -in $Cases) } else { @($matrix.cases) }
if (-not $IsWindows) { throw 'Real-host qualification requires Windows and an interactive desktop; no tests passed' }
if (-not $AllowInputInjection) { throw 'Read scripts/windows_input/README.md, then explicitly pass -AllowInputInjection on an isolated desktop' }
if (-not [Environment]::UserInteractive) { throw 'Interactive desktop unavailable' }
if ($Widths.Count -eq 0 -or $Heights.Count -eq 0 -or @($Widths | Where-Object { $_ -lt 40 -or $_ -gt 500 }).Count -or @($Heights | Where-Object { $_ -lt 15 -or $_ -gt 150 }).Count) { throw 'Invalid geometry selection' }
Add-Type -Path "$PSScriptRoot/windows_input/Native.cs"
[HerdrInputGauntlet.Desktop]::AssertNotElevated($PID)
[HerdrInputGauntlet.Desktop]::Neutral()
$controllerWindow = [IntPtr]::Zero
if (@($selectedCases | Where-Object id -eq 'mouse-focus-refresh').Count) {
    $controllerWindow = [HerdrInputGauntlet.Desktop]::GetForegroundWindow()
    if ($controllerWindow -eq [IntPtr]::Zero) { throw 'Controller has no foreground window for focus-cycle qualification' }
    [HerdrInputGauntlet.Desktop]::AssertNotElevated([HerdrInputGauntlet.Desktop]::Pid($controllerWindow))
}
$sourceCommit = $null
$sourceDirty = $null
if (-not $ExePath) {
    $repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
    $sourceCommit = (& git -C $repo rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Could not identify the source checkout commit' }
    $sourceDirty = [bool](& git -C $repo status --porcelain)
    Write-Host "Building current checkout: $repo"
    Push-Location $repo
    try {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally { Pop-Location }
    $targetRoot = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR, $repo) } else { Join-Path $repo 'target' }
    $builtExe = Join-Path $targetRoot 'release/herdr.exe'
    $package = Join-Path $repo '.local/windows-input/cache/Microsoft.Windows.Console.ConPTY.nupkg'
    $stage = Join-Path $root 'package'
    Write-Host 'Staging the current binary with the pinned ConPTY runtime'
    $null = Invoke-GauntletProcess $python @((Join-Path $PSScriptRoot 'package_windows_conpty.py'), 'stage', '--package', $package, '--herdr-exe', $builtExe, '--output-dir', $stage) -Timeout 300
    $ExePath = Join-Path $stage 'herdr.exe'
}
$exe = (Resolve-Path -LiteralPath $ExePath).Path
$conpty = Join-Path ([IO.Path]::GetDirectoryName($exe)) 'conpty/conpty.dll'
if (-not (Test-Path -LiteralPath $conpty -PathType Leaf)) { throw "Selected Herdr binary has no adjacent bundled ConPTY runtime: $exe. Pass -ExePath to a packaged herdr.exe" }
$pwsh = (Get-Process -Id $PID).Path
$exeHash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
if ($sourceCommit) { Write-Host "Source: $sourceCommit$(if ($sourceDirty) { ' + working tree changes' })" }
Write-Host "Herdr under test: $exe"
Write-Host "SHA-256: $exeHash"
Write-Host "Modes: $($Modes -join ', '); widths: $($Widths -join ', '); heights: $($Heights -join ', ')"
Write-Host "Channels: $($Channels -join ', '); paths: $($Paths -join ', '); cases: $(if (@($Cases).Count) { $Cases -join ', ' } else { 'all' })"
Write-Host "Clipboard formats at start: $([HerdrInputGauntlet.Desktop]::CountClipboardFormats()) (paste runs only when zero)"
$document = @{ schema = 1; run = [IO.Path]::GetFileName($root); started = [DateTime]::UtcNow.ToString('O');
    source_commit = $sourceCommit; source_dirty = $sourceDirty; exe = $exe; exe_sha256 = $exeHash;
    powershell = $PSVersionTable.PSVersion.ToString(); os = [Environment]::OSVersion.VersionString;
    profile = $Profile; controller_elevated = $false; observations = @(); hosts = @(); errors = @(); cleanup_errors = @();
    widths = $Widths; heights = $Heights; modes = $Modes; channels = $Channels; paths = $Paths;
    cases = @($selectedCases | ForEach-Object id); note = 'Desktop input evidence; native qualification still required' }
$rawReport = Join-Path $root 'observations.json'
$artifactReport = Join-Path $root 'report.json'

function Save-Report { Write-GauntletJson $rawReport $document }
function Resolve-Terminal($Name, $Override) {
    if ($Override) { return @{ path = (Resolve-Path -LiteralPath $Override).Path; app_user_model_id = $null } }
    $packageName = if ($Name -eq 'stable') { 'Microsoft.WindowsTerminal' } else { 'Microsoft.WindowsTerminalPreview' }
    $package = @(Get-AppxPackage -Name $packageName -ErrorAction SilentlyContinue | Sort-Object Version -Descending)
    if ($package.Count -eq 0) { return $null }
    $path = Join-Path $package[0].InstallLocation 'WindowsTerminal.exe'
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    return @{ path = $path; app_user_model_id = "$($package[0].PackageFamilyName)!App" }
}
function Observer-Request($Plan, $Action, $Value = $null) {
    $id = [guid]::NewGuid().ToString('N')
    Write-GauntletJson (Join-Path $Plan.work 'request.json') @{ id = $id; nonce = $Plan.nonce; action = $Action; value = $Value }
    $reply = Wait-GauntletJson (Join-Path $Plan.work 'reply.json') $root { param($v) $v.id -eq $id -and $v.nonce -eq $Plan.nonce -and $v.action -eq $Action }
    if ($reply.error) { throw "Observer failed: $($reply.error)" }
    return $reply
}
function Outer-State($Plan) {
    $after = if ($Plan.ContainsKey('outer_sequence')) { $Plan.outer_sequence } else { 0 }
    $state = Wait-GauntletJson (Join-Path $Plan.work 'outer.json') $root { param($v) $v.nonce -eq $Plan.nonce -and $v.sequence -gt $after }
    $Plan.outer_sequence = $state.sequence
    return $state
}
function Get-GauntletClientTrace($Plan) {
    # The client writes its mapper trace into the named session's data directory.
    # The client log grows independently of the transport log, so this keeps its
    # own cursor rather than reusing the transport line count.
    $log = Join-Path $Plan.config_home "herdr/sessions/$($Plan.session)/herdr-client.log"
    if (-not (Test-Path -LiteralPath $log)) { return $null }
    $lines = @(Get-Content -LiteralPath $log | Where-Object { $_ -like '*windows input trace: input batch*' })
    $since = if ($Plan.ContainsKey('client_trace_lines')) { [int]$Plan.client_trace_lines } else { 0 }
    $Plan.client_trace_lines = $lines.Count
    if ($lines.Count -le $since) { return @() }
    return @($lines | Select-Object -Skip $since)
}
function Set-ObservedGeometry($Plan, $Window, $WindowPid, $Width, $Height) {
    # Correct against actual console cell measurements, not a claimed pixel size.
    for ($attempt = 0; $attempt -lt 8; $attempt++) {
        [HerdrInputGauntlet.Desktop]::Guard($Window, $Plan.nonce, $WindowPid)
        $outer = Outer-State $Plan
        $w = [int]$outer.geometry[0]; $h = [int]$outer.geometry[1]
        if ($w -eq $Width -and $h -eq $Height) { return $outer }
        [HerdrInputGauntlet.Desktop]::Resize($Window, $Plan.nonce, $WindowPid, ($Width - $w) * 8, ($Height - $h) * 16)
        Start-Sleep -Milliseconds 350
        Update-GauntletLease $root
    }
    return $null
}
function New-Observation($HostName, $Plan, $Width, $Height, $Case) {
    return @{ host = $HostName; path = $Plan.path; mode = $Plan.mode; width = $Width; height = $Height; case = $Case.id;
        nonce = $Plan.nonce; status = 'inconclusive'; ready = $false; focus_verified = $false; complete = $false }
}

try {
    [HerdrInputGauntlet.Desktop]::StartEmergencyStop()
    $terminals = @{ stable = (Resolve-Terminal 'stable' $StablePath); preview = (Resolve-Terminal 'preview' $PreviewPath) }
    foreach ($channel in $Channels) {
        $terminal = $terminals[$channel]
        if ($terminal) { Write-Host "Windows Terminal $channel`: $($terminal.path) ($((Get-Item $terminal.path).VersionInfo.FileVersion))" }
        else { Write-Host "Windows Terminal $channel`: not installed (channel will be not_run)" }
    }
    $launcherIdentities = @{}; $installationIdentities = @{}
    foreach ($channel in $Channels) {
        if ($terminals[$channel]) {
            $launcherIdentities[$channel] = [HerdrInputGauntlet.Desktop]::FileIdentity($terminals[$channel].path)
            $installationIdentities[$channel] = [HerdrInputGauntlet.Desktop]::FileIdentity([IO.Path]::GetDirectoryName($terminals[$channel].path))
        }
    }
    if ($Channels -contains 'stable' -and $Channels -contains 'preview' -and $terminals.stable -and $terminals.preview -and ($launcherIdentities.stable -eq $launcherIdentities.preview -or $installationIdentities.stable -eq $installationIdentities.preview)) {
        throw 'Stable and Preview resolve to the same executable/installation; refusing duplicate channel evidence'
    }
    foreach ($hostName in $Channels) {
        $terminal = $terminals[$hostName]
        if (-not $terminal) {
            $document.hosts += @{ channel = $hostName; status = 'not_run'; reason = 'Terminal installation not found; supply explicit path' }
            Save-Report
            continue
        }
        $hostRecord = @{ channel = $hostName; launcher = $terminal.path; launcher_version = (Get-Item $terminal.path).VersionInfo.FileVersion;
            launcher_identity = $launcherIdentities[$hostName]; installation_identity = $installationIdentities[$hostName]; runs = @() }
        $document.hosts += $hostRecord
        foreach ($path in $Paths) { foreach ($mode in $Modes) {
            $nonce = 'herdr-gauntlet-' + [guid]::NewGuid().ToString('N')
            $work = [IO.Directory]::CreateDirectory((Join-Path $root $nonce)).FullName
            $configHome = [IO.Directory]::CreateDirectory((Join-Path $work 'config')).FullName
            $config = Join-Path $configHome 'test.toml'
            $shellToml = $pwsh | ConvertTo-Json -Compress
            [IO.File]::WriteAllText($config, "onboarding = false`n[terminal]`ndefault_shell = $shellToml`n[ui]`nmouse_capture = true`n")
            $plan = @{ root = $root; work = $work; nonce = $nonce; session = $nonce; config = $config; config_home = $configHome;
                path = $path; mode = $mode; profile = $Profile; exe = $exe; pwsh = $pwsh; input_trace = (Join-Path $work 'input-transport.log') }
            $planPath = Join-Path $work 'plan.json'
            Write-GauntletJson $planPath $plan
            $window = [IntPtr]::Zero; $windowPid = 0; $server = $null; $launcher = $null; $clipboardSequence = $null; $injectionAuthorized = $false
            $cursorPosition = $null; $mouseReporting = $false
            Update-GauntletLease $root
            try {
                if ($path -in @('herdr', 'herdr-remote')) {
                    $server = New-GauntletProcess $exe @('--session', $nonce, 'server') $plan -Capture
                    # Drain pipes asynchronously; server output must never block readiness.
                    $serverOut = $server.StandardOutput.ReadToEndAsync(); $serverErr = $server.StandardError.ReadToEndAsync()
                    $deadline = [DateTime]::UtcNow.AddSeconds(30)
                    do {
                        Update-GauntletLease $root
                        try { $status = Invoke-GauntletProcess $exe @('--session', $nonce, 'status', 'server') $plan -Timeout 5 } catch { $status = '' }
                        if ($status -match 'status: running') { break }
                        Start-Sleep -Milliseconds 200
                    } while ([DateTime]::UtcNow -lt $deadline)
                    if ($status -notmatch 'status: running') { throw 'Owned server did not become ready' }
                    $created = (Invoke-GauntletProcess $exe @('--session', $nonce, 'workspace', 'create', '--cwd', $work, '--focus') $plan) | ConvertFrom-Json
                    $pane = $created.result.root_pane.pane_id
                    if (-not $pane) { throw 'Missing owned probe pane identity' }
                    $command = '& ' + (Quote-GauntletPS $pwsh) + ' -NoProfile -File ' + (Quote-GauntletPS "$PSScriptRoot/windows_input/Probe.ps1") + ' -PlanPath ' + (Quote-GauntletPS $planPath)
                    $null = Invoke-GauntletProcess $exe @('--session', $nonce, 'pane', 'run', $pane, $command) $plan
                }
                # Always request a new window. Bootstrap independently clears inherited identity.
                $terminalArguments = @('-w', 'new', 'new-tab', '--title', $nonce, '--suppressApplicationTitle', '--', $pwsh, '-NoProfile', '-File', "$PSScriptRoot/windows_input/Bootstrap.ps1", '-PlanPath', $planPath)
                if ($terminal.app_user_model_id) {
                    $launcher = Get-Process -Id ([HerdrInputGauntlet.Desktop]::ActivateApplication($terminal.app_user_model_id, $terminalArguments)) -ErrorAction SilentlyContinue
                } else {
                    $launcher = New-GauntletProcess $terminal.path $terminalArguments $plan
                }
                $deadline = [DateTime]::UtcNow.AddSeconds(25)
                do {
                    Update-GauntletLease $root
                    $window = [HerdrInputGauntlet.Desktop]::Find($nonce)
                    if ($window -ne [IntPtr]::Zero) { break }
                    Start-Sleep -Milliseconds 100
                } while ([DateTime]::UtcNow -lt $deadline)
                if ($window -eq [IntPtr]::Zero) { throw 'No unique owned Terminal window appeared' }
                $windowPid = [HerdrInputGauntlet.Desktop]::Pid($window)
                $windowProcess = Get-Process -Id $windowPid
                if ($windowProcess.ProcessName -ne 'WindowsTerminal') { throw 'Nonce window is not Windows Terminal; refusing injection' }
                [HerdrInputGauntlet.Desktop]::AssertNotElevated($windowPid)
                $imageIdentity = [HerdrInputGauntlet.Desktop]::FileIdentity($windowProcess.Path)
                $installationIdentity = [HerdrInputGauntlet.Desktop]::FileIdentity([IO.Path]::GetDirectoryName($windowProcess.Path))
                $processIdentity = "$windowPid/$($windowProcess.StartTime.ToUniversalTime().Ticks)"
                foreach ($otherHost in $document.hosts) {
                    if ($otherHost.channel -ne $hostName -and $otherHost.ContainsKey('runs')) {
                        foreach ($other in $otherHost.runs) {
                            if ($other.image_identity -eq $imageIdentity -or $other.installation_identity -eq $installationIdentity -or $other.process_identity -eq $processIdentity) {
                                throw 'Stable and Preview activated the same Terminal installation/process; refusing duplicate evidence'
                            }
                        }
                    }
                }
                $runRecord = @{ nonce = $nonce; path = $path; mode = $mode; hwnd = $window.ToInt64(); pid = $windowPid;
                    terminal_path = $windowProcess.Path; terminal_version = $windowProcess.MainModule.FileVersionInfo.FileVersion;
                    elevated = $false; image_identity = $imageIdentity; installation_identity = $installationIdentity; process_identity = $processIdentity;
                    layout = [HerdrInputGauntlet.Desktop]::Layout($window) }
                $hostRecord.runs += $runRecord
                $ready = Wait-GauntletJson (Join-Path $work 'ready.json') $root { param($v) $v.nonce -eq $nonce -and $v.mode -eq $mode }
                [HerdrInputGauntlet.Desktop]::Focus($window, $nonce, $windowPid)
                $injectionAuthorized = $true
                Start-Sleep -Milliseconds 500
                # Full case catalogue once; boundary geometries run discriminating sentinels.
                $geometries = @(@{ width = 120; height = 30; full = $true })
                foreach ($height in $Heights) { foreach ($width in $Widths) { $geometries += @{ width = $width; height = $height; full = $false } } }
                # Return narrow after wide to exercise repeated reflow/recovery.
                $geometries += @{ width = 80; height = 30; full = $false }
                $phase = 0
                foreach ($geometry in $geometries) {
                    $phase++
                    $selected = if ($geometry.full) { $selectedCases } else { @($selectedCases | Where-Object { $_.id -in @('letter-a', 'shift-enter', 'paste-lf', 'mouse-focus-refresh') }) }
                    $outer = Set-ObservedGeometry $plan $window $windowPid $geometry.width $geometry.height
                    foreach ($case in $selected) {
                        $row = New-Observation $hostName $plan $geometry.width $geometry.height $case
                        $row.phase = $phase
                        $document.observations += $row
                        if ($case.kind -eq 'qualification' -or -not $case.expected.ContainsKey($mode)) { $row.status = 'not_run'; $row.reason = 'Requires separate qualification: ' + $case.id; continue }
                        if ($case.kind -eq 'manual' -and -not $Manual) { $row.status = 'not_run'; $row.reason = 'Operator-assisted case; rerun with -Manual and declared layout'; continue }
                        $layoutChords = $null
                        if ($case.kind -eq 'layout-key') {
                            try {
                                $layoutChords = @([HerdrInputGauntlet.Desktop]::DeadKeyChord($window, [char]$case.dead), [int[]]@([int]$case.base_vk))
                                $row.input_layout = [HerdrInputGauntlet.Desktop]::Layout($window)
                            } catch {
                                $row.status = 'not_run'
                                $row.reason = if ($_.Exception.InnerException) { $_.Exception.InnerException.Message } else { $_.Exception.Message }
                                continue
                            }
                        }
                        if ($null -eq $outer) { $row.status = 'not_run'; $row.reason = 'Requested cell geometry not reached; monitor/font/window constraints'; continue }
                        $fresh = Outer-State $plan
                        if ($fresh.geometry[0] -ne $geometry.width -or $fresh.geometry[1] -ne $geometry.height) {
                            $fresh = Set-ObservedGeometry $plan $window $windowPid $geometry.width $geometry.height
                            if ($null -eq $fresh) { $row.status = 'inconclusive'; $row.reason = 'Geometry changed and could not be restored'; continue }
                        }
                        $row.outer_geometry = $fresh.geometry
                        $row.outer_sequence = $fresh.sequence
                        $row.negotiation_hex = $ready.negotiation_hex
                        if ($mode -eq 'kitty' -and -not $ready.kitty_acknowledged) { $row.status = 'inconclusive'; $row.reason = 'Kitty disambiguation query not acknowledged; host support is not established'; continue }
                        if ($case.kind -in @('paste', 'mouse-interleave', 'clipboard-image', 'clipboard-mixed') -and [HerdrInputGauntlet.Desktop]::CountClipboardFormats() -ne 0) {
                            $row.status = 'not_run'; $row.reason = 'Clipboard is not empty; refusing to replace user data'; continue
                        }
                        if ($case.kind -in @('mouse-interleave', 'mouse-focus-refresh')) { $null = Observer-Request $plan 'mouse-on'; $mouseReporting = $true }
                        $traceLineCount = if ($path -in @('herdr', 'herdr-remote') -and (Test-Path -LiteralPath $plan.input_trace)) { @(Get-Content -LiteralPath $plan.input_trace).Count } else { 0 }
                        $begin = Observer-Request $plan 'begin'
                        $row.ready = $true; $row.pane_geometry = $begin.geometry
                        [HerdrInputGauntlet.Desktop]::Guard($window, $nonce, $windowPid)
                        $row.focus_verified = $true
                        if ($case.kind -eq 'key') {
                            $row.scans = @([HerdrInputGauntlet.Desktop]::Scans($window, [int[]]$case.chords[0]))
                            foreach ($chord in $case.chords) { $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]$chord) }
                        } elseif ($case.kind -eq 'paste') {
                            $clipboardSequence = [HerdrInputGauntlet.Desktop]::SetEmptyClipboard($window, $case.text)
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(17, 86))
                        } elseif ($case.kind -in @('clipboard-image', 'clipboard-mixed')) {
                            $text = if ($case.kind -eq 'clipboard-mixed') { [string]$case.text } else { $null }
                            $clipboardSequence = [HerdrInputGauntlet.Desktop]::SetEmptyClipboardImage($window, $text)
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(17, 86))
                        } elseif ($case.kind -eq 'mouse-interleave') {
                            $cursorPosition = [HerdrInputGauntlet.Desktop]::Cursor()
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(65))
                            [HerdrInputGauntlet.Desktop]::MouseMoveInside($window, $nonce, $windowPid, -60)
                            Start-Sleep -Milliseconds 150
                            $clipboardSequence = [HerdrInputGauntlet.Desktop]::SetEmptyClipboard($window, $case.text)
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(17, 86))
                            Start-Sleep -Milliseconds 150
                            [HerdrInputGauntlet.Desktop]::MouseMoveInside($window, $nonce, $windowPid, 60)
                            Start-Sleep -Milliseconds 150
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(66))
                        } elseif ($case.kind -eq 'mouse-focus-refresh') {
                            $cursorPosition = [HerdrInputGauntlet.Desktop]::Cursor()
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(65))
                            [HerdrInputGauntlet.Desktop]::MouseClickWheelInside($window, $nonce, $windowPid, -60)
                            Start-Sleep -Milliseconds 200
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(66))
                            [HerdrInputGauntlet.Desktop]::FocusAwayAndBack($controllerWindow, $window, $nonce, $windowPid)
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(67))
                            [HerdrInputGauntlet.Desktop]::MouseClickWheelInside($window, $nonce, $windowPid, 60)
                            Start-Sleep -Milliseconds 200
                            $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(68))
                        } elseif ($case.kind -eq 'mode-transitions') {
                            foreach ($step in @(
                                @{ mode = 'legacy'; chords = @([int[]]@(65), [int[]]@(16, 13)) },
                                @{ mode = 'mok2'; chords = @([int[]]@(66), [int[]]@(16, 13)) },
                                @{ mode = 'kitty'; chords = @([int[]]@(67), [int[]]@(16, 13)) },
                                @{ mode = 'mok2'; chords = @([int[]]@(68), [int[]]@(16, 13)) },
                                @{ mode = 'legacy'; chords = @([int[]]@(69), [int[]]@(16, 13), [int[]]@(70)) }
                            )) {
                                if ($step.mode) { $null = Observer-Request $plan 'set-mode' $step.mode; Start-Sleep -Milliseconds 200 }
                                foreach ($chord in $step.chords) { $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]$chord) }
                            }
                        } elseif ($case.kind -eq 'layout-key') {
                            foreach ($chord in $layoutChords) { $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]$chord) }
                        } else {
                            Write-Host "$($case.id): $($case.prompt) Return to this controller and press Enter after the gesture."
                            # Manual waits are bounded by the independent bootstrap lease watchdog.
                            $null = Read-Host
                            $row.operator_layout = [HerdrInputGauntlet.Desktop]::Layout($window)
                        }
                        Start-Sleep -Milliseconds 500
                        if ($case.kind -ne 'manual') { [HerdrInputGauntlet.Desktop]::Guard($window, $nonce, $windowPid) }
                        $end = Observer-Request $plan 'end'
                        if ($case.kind -eq 'mode-transitions') { $null = Observer-Request $plan 'set-mode' $mode }
                        $row.capture_id = $end.id
                        $row.hex = $end.hex; $row.records = $end.records; $row.error = $end.error; $row.complete = $end.quiet_reached
                        if ($case.kind -eq 'clipboard-image' -and $path -eq 'herdr-remote') {
                            $capture = [Convert]::FromHexString([string]$end.hex)
                            if ($capture.Length -ge 12) {
                                $stagedPath = [Text.Encoding]::UTF8.GetString($capture, 6, $capture.Length - 12)
                                if (Test-Path -LiteralPath $stagedPath -PathType Leaf) {
                                    $row.staged_image_sha256 = (Get-FileHash -LiteralPath $stagedPath -Algorithm SHA256).Hash
                                }
                            }
                        }
                        $row.final_outer_geometry = (Outer-State $plan).geometry
                        $row.status = 'observed'; $row.final_pane_geometry = $end.geometry
                        if ($path -in @('herdr', 'herdr-remote') -and (Test-Path -LiteralPath $plan.input_trace)) {
                            $traceLines = @(Get-Content -LiteralPath $plan.input_trace)
                            $trace = $traceLines -join "`n"
                            $captureTrace = ($traceLines | Select-Object -Skip $traceLineCount) -join "`n"
                            $row.input_reader = if ($trace.Contains('reader=windows-console')) { 'windows-console' } elseif ($trace.Contains('reader=crossterm')) { 'crossterm' } else { 'unknown' }
                            if ($captureTrace.Contains('transport=win32-serialized')) { $row.input_transport = 'win32-serialized' }
                        }
                        if ($path -in @('herdr', 'herdr-remote')) {
                            # The mapper's own client-event view disambiguates a paste the
                            # terminal issued from a remote-image-bridge reaction (#4314).
                            $clientEvents = Get-GauntletClientTrace $plan
                            if ($null -ne $clientEvents) { $row.client_events = $clientEvents }
                        }
                        if ($null -ne $clipboardSequence) {
                            if (-not [HerdrInputGauntlet.Desktop]::ClearOwnedClipboard($window, $clipboardSequence)) { throw 'Could not clear test-owned clipboard' }
                            $clipboardSequence = $null
                        }
                        if ($mouseReporting) { $null = Observer-Request $plan 'mouse-off'; $mouseReporting = $false }
                        if ($null -ne $cursorPosition) { [HerdrInputGauntlet.Desktop]::RestoreCursor($cursorPosition); $cursorPosition = $null }
                        if ($case.kind -eq 'manual') { [HerdrInputGauntlet.Desktop]::Focus($window, $nonce, $windowPid) }
                        Save-Report
                        Update-GauntletLease $root
                    }
                }
            } catch {
                $document.errors += "$nonce : $($_.Exception.Message)"
                Save-Report
                # Abort the campaign on lost focus, readiness failure or partial injection.
                throw
            } finally {
                if ($mouseReporting) {
                    try { $null = Observer-Request $plan 'mouse-off'; $mouseReporting = $false } catch { $document.cleanup_errors += $_.Exception.Message }
                }
                if ($null -ne $cursorPosition -and [HerdrInputGauntlet.Desktop]::GetForegroundWindow() -eq $window) {
                    [HerdrInputGauntlet.Desktop]::RestoreCursor($cursorPosition); $cursorPosition = $null
                }
                if ($null -ne $clipboardSequence -and -not [HerdrInputGauntlet.Desktop]::ClearOwnedClipboard($window, $clipboardSequence)) { $document.cleanup_errors += 'Could not clear test-owned clipboard' }
                [IO.File]::WriteAllText((Join-Path $work 'probe-stop'), '')
                if (Test-Path (Join-Path $work 'ready.json')) {
                    try {
                        $probeExit = Wait-GauntletJson (Join-Path $work 'probe-exit.json') $root { param($v) $v.nonce -eq $nonce } -Timeout 5
                        $document.cleanup_errors += @($probeExit.cleanup_errors)
                    } catch { $document.cleanup_errors += $_.Exception.Message }
                }
                # Normal detach only while still owning foreground focus; never type into another window.
                if ($injectionAuthorized -and $path -in @('herdr', 'herdr-remote') -and [HerdrInputGauntlet.Desktop]::IsOwned($window, $nonce, $windowPid) -and [HerdrInputGauntlet.Desktop]::GetForegroundWindow() -eq $window) {
                    try {
                        $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(17, 66))
                        Start-Sleep -Milliseconds 100
                        $null = [HerdrInputGauntlet.Desktop]::Chord($window, $nonce, $windowPid, [int[]]@(81))
                    } catch { $document.cleanup_errors += $_.Exception.Message }
                }
                if ($null -ne $server -and -not $server.HasExited) {
                    try { $null = Invoke-GauntletProcess $exe @('session', 'stop', $nonce) $plan }
                    catch { if (-not $server.WaitForExit(5000)) { $document.cleanup_errors += $_.Exception.Message } }
                }
                [IO.File]::WriteAllText((Join-Path $work 'stop'), '')
                if ($window -ne [IntPtr]::Zero) {
                    try {
                        $exitRecord = Wait-GauntletJson (Join-Path $work 'bootstrap-exit.json') $root { param($v) $v.nonce -eq $nonce } -Timeout 45
                        $document.cleanup_errors += @($exitRecord.cleanup_errors)
                        if ($exitRecord.before[2] -ne $exitRecord.after[2]) { $document.cleanup_errors += "$nonce console input mode not restored" }
                    } catch { $document.cleanup_errors += $_.Exception.Message }
                    $windowDeadline = [DateTime]::UtcNow.AddSeconds(5)
                    while ([HerdrInputGauntlet.Desktop]::IsOwned($window, $nonce, $windowPid) -and [DateTime]::UtcNow -lt $windowDeadline) { Start-Sleep -Milliseconds 100 }
                    if ([HerdrInputGauntlet.Desktop]::IsOwned($window, $nonce, $windowPid)) { $document.cleanup_errors += "$nonce Terminal window remained open; close it manually" }
                }
                if ($null -ne $server) {
                    if (-not $server.WaitForExit(5000)) {
                        $server.Kill($true)
                        $document.cleanup_errors += "$nonce server required forced cleanup"
                        if (-not $server.WaitForExit(5000)) { $document.cleanup_errors += "$nonce server remained active after forced cleanup" }
                    }
                    try { $null = Invoke-GauntletProcess $exe @('session', 'delete', $nonce) $plan } catch { $document.cleanup_errors += $_.Exception.Message }
                    $server.Dispose()
                }
                if ($null -ne $launcher) { $launcher.Dispose() }
                Save-Report
            }
        } }
    }
} catch {
    if ($document.errors.Count -eq 0) { $document.errors += $_.Exception.Message }
} finally { [HerdrInputGauntlet.Desktop]::StopEmergencyStop(); Save-Report }
& $python $reportScript report --input $rawReport --output $artifactReport
$code = $LASTEXITCODE
Write-Host "Evidence and report: $root"
exit $code
