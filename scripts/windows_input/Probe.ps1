#Requires -Version 7.0
param([Parameter(Mandatory)][string] $PlanPath)
. "$PSScriptRoot/Common.ps1"
$plan = Read-GauntletJson $PlanPath
Add-Type -Path "$PSScriptRoot/Native.cs"
$probe = $null
try {
    $probe = [HerdrInputGauntlet.ConsoleProbe]::new($plan.mode)
    [HerdrInputGauntlet.ConsoleProbe]::Print("`r`nHerdr input gauntlet observer $($plan.nonce)`r`n")
    if ($plan.mode -eq 'kitty') {
        $deadline = [DateTime]::UtcNow.AddSeconds(2)
        while (-not $probe.Hex().Contains('1b5b3f3175') -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    }
    Write-GauntletJson (Join-Path $plan.work 'ready.json') @{
        nonce = $plan.nonce; pid = $PID; mode = $plan.mode; geometry = [HerdrInputGauntlet.ConsoleProbe]::Geometry()
        negotiation_hex = $probe.Hex(); kitty_acknowledged = $probe.Hex().Contains('1b5b3f3175')
    }
    # Negotiation replies are setup evidence, not input for the first case.
    $probe.Clear()
    $closedCount = 0
    $last = ''
    while ((Test-GauntletLease $plan.root) -and -not (Test-Path (Join-Path $plan.work 'probe-stop'))) {
        $request = Read-GauntletJson (Join-Path $plan.work 'request.json')
        if ($null -ne $request -and $request.id -ne $last) {
            if ($request.nonce -ne $plan.nonce) { throw 'Unexpected observer nonce' }
            $last = $request.id
            $quietReached = $false
            if ($request.action -eq 'mouse-on') {
                [HerdrInputGauntlet.ConsoleProbe]::Print("`e[?1003h`e[?1006h")
                Start-Sleep -Milliseconds 200
            }
            if ($request.action -eq 'mouse-off') {
                [HerdrInputGauntlet.ConsoleProbe]::Print("`e[?1003l`e[?1006l")
                Start-Sleep -Milliseconds 100
            }
            if ($request.action -eq 'set-mode') {
                $probe.SetKeyboardMode([string]$request.value)
                Start-Sleep -Milliseconds 200
            }
            if ($request.action -eq 'begin' -and -not $probe.ClearIfCount($closedCount)) { throw 'Unexpected input arrived between captures' }
            if ($request.action -eq 'end') {
                # Capture trailing duplicates/releases as well as the first expected bytes.
                $deadline = [DateTime]::UtcNow.AddSeconds(3)
                $quiet = [DateTime]::UtcNow
                $count = $probe.Count()
                do {
                    Start-Sleep -Milliseconds 50
                    $newCount = $probe.Count()
                    if ($newCount -ne $count) { $quiet = [DateTime]::UtcNow; $count = $newCount }
                } while (([DateTime]::UtcNow - $quiet).TotalMilliseconds -lt 350 -and [DateTime]::UtcNow -lt $deadline)
                $quietReached = ([DateTime]::UtcNow - $quiet).TotalMilliseconds -ge 350
                $closedCount = $probe.Count()
            }
            Write-GauntletJson (Join-Path $plan.work 'reply.json') @{
                id = $request.id; nonce = $plan.nonce; action = $request.action
                hex = $probe.Hex(); records = @($probe.Records()); error = $probe.Error; quiet_reached = $quietReached
                geometry = [HerdrInputGauntlet.ConsoleProbe]::Geometry()
            }
        }
        Start-Sleep -Milliseconds 25
    }
} catch {
    Write-GauntletJson (Join-Path $plan.work 'probe-error.json') @{ error = $_.ToString(); nonce = $plan.nonce }
    throw
} finally {
    if ($null -ne $probe) {
        $probe.Dispose()
        Write-GauntletJson (Join-Path $plan.work 'probe-exit.json') @{ nonce = $plan.nonce; cleanup_errors = @($probe.CleanupErrors) }
    }
}
