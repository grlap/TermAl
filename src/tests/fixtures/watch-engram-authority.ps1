# Read-only sampling of test-fixture authority processes, not a test wait or
# synchronization mechanism. Run in a separate shell during the Rust gate.
# Samples go to an ignored diagnostic log; no process is stopped or modified.
param(
    [string]$OutputPath = "target/engram-authority-watch.jsonl",
    [ValidateRange(1, 100000)][int]$Samples = 240,
    [ValidateRange(1, 60000)][int]$IntervalMs = 500,
    [ValidateRange(0, 3600)][double]$MinimumAgeSeconds = 0.5
)

$ErrorActionPreference = "Stop"
$samplePath = [IO.Path]::GetFullPath($OutputPath)
if (-not (Test-Path -LiteralPath (Split-Path -Parent $samplePath) -PathType Container)) {
    throw "Diagnostic output directory must already exist: $samplePath"
}
for ($sampleIndex = 0; $sampleIndex -lt $Samples; $sampleIndex++) {
    $powershellProcesses = @(Get-CimInstance Win32_Process -Filter "Name = 'powershell.exe'")
    foreach ($fixtureProcess in $powershellProcesses) {
        $commandLine = $fixtureProcess.CommandLine
        if ($commandLine -notmatch 'engram-control-fixture\.ps1' -or
            $commandLine -notmatch ' authority revoke ') {
            continue
        }
        $age = ((Get-Date) - $fixtureProcess.CreationDate).TotalSeconds
        if ($age -lt $MinimumAgeSeconds) { continue }
        $liveProcess = Get-Process -Id $fixtureProcess.ProcessId -ErrorAction SilentlyContinue
        if ($null -eq $liveProcess) { continue }
        $fixtureHome = [regex]::Match($commandLine, '--home (.+?) authority revoke').Groups[1].Value.Trim('"')
        $phaseText = "<not created>"
        if ($fixtureHome) {
            $phasePath = Join-Path $fixtureHome "authority-revoke-phases"
            # A fixture may finish and delete its temp root between samples.
            $phaseText = Get-Content -Raw -LiteralPath $phasePath -ErrorAction SilentlyContinue
            if ($null -eq $phaseText) { $phaseText = "<not created>" }
        }
        try {
            $threadStates = @($liveProcess.Threads | ForEach-Object {
                [ordered]@{
                    id = $_.Id
                    state = $_.ThreadState.ToString()
                    waitReason = if ($_.ThreadState -eq [Diagnostics.ThreadState]::Wait) {
                        $_.WaitReason.ToString()
                    } else { $null }
                }
            })
        } catch {
            # Thread exit can race the read; retain the other sampled evidence.
            $threadStates = @(@{ unavailable = $_.Exception.Message })
        }
        [ordered]@{
            at = [DateTime]::UtcNow.ToString("o")
            pid = $fixtureProcess.ProcessId
            parentPid = $fixtureProcess.ParentProcessId
            ageSeconds = $age
            cpuSeconds = $liveProcess.CPU
            powershellCount = $powershellProcesses.Count
            fixtureHome = $fixtureHome
            phases = $phaseText
            threads = $threadStates
        } | ConvertTo-Json -Depth 6 -Compress | Tee-Object -FilePath $samplePath -Append
    }
    Start-Sleep -Milliseconds $IntervalMs
}
