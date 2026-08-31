$ErrorActionPreference = "Stop"

$projectFile = $null
$engramHome = $null
$actorId = $null
$sessionId = $null
$operation = $null
$workRef = $null
$sections = $null
for ($index = 0; $index -lt $args.Count; $index += 1) {
    switch ($args[$index]) {
        "--project-file" { $projectFile = $args[++$index] }
        "--home" { $engramHome = $args[++$index] }
        "--actor-id" { $actorId = $args[++$index] }
        "--session-id" { $sessionId = $args[++$index] }
        "--sections" { $sections = $args[++$index] }
        "next" { $operation = "next" }
        "focus" {
            if ($operation -eq "next") {
                $sections = "focus"
            } else {
                $operation = "focus"
                if ($index + 1 -lt $args.Count) {
                    $workRef = $args[$index + 1]
                }
            }
        }
    }
}

if (-not $projectFile -or -not $engramHome -or -not $actorId -or -not $sessionId) {
    exit 2
}
if ($actorId -ne "termal" -or $sessionId -ne "fixture-session") {
    exit 3
}
$mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
$marker = Join-Path $engramHome "work-next-read"
$lockRetryMarker = Join-Path $engramHome "work-lock-retried"
if ($operation -eq "next") {
    if ($sections -ne "focus") {
        exit 4
    }
    if ($mode -eq "read-error") {
        [Console]::Error.WriteLine("database is locked")
        exit 6
    }
    if ($mode -eq "read-error-once" -and -not (Test-Path -LiteralPath $lockRetryMarker)) {
        Set-Content -LiteralPath $lockRetryMarker -Value "retry" -NoNewline
        [Console]::Error.WriteLine("database is locked")
        exit 6
    }
    Set-Content -LiteralPath $marker -Value "ready" -NoNewline
    if ($mode -eq "no-focus") {
        [Console]::Out.WriteLine('{"session":{},"focus":null}')
    } else {
        [Console]::Out.WriteLine('{"session":{},"focus":{"status":{"work":{"work_id":"work-fixture"}}}}')
    }
    exit 0
}
if ($operation -eq "focus" -and $workRef -eq "work-fixture" -and (Test-Path -LiteralPath $marker)) {
    [Console]::Out.WriteLine('{"control_binding":{"root_execution_id":"root-fixture","work_id":"work-fixture","run_id":"run-fixture","work_revision":17,"claim_id":"claim-fixture","claim_fence":23}}')
    exit 0
}
exit 5
