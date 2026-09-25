$ErrorActionPreference = "Stop"

$projectFile = $null
$engramHome = $null
$actorId = $null
$actorContext = $null
$sessionId = $null
$operation = $null
for ($index = 0; $index -lt $args.Count; $index += 1) {
    switch ($args[$index]) {
        "--project-file" { $projectFile = $args[++$index] }
        "--home" { $engramHome = $args[++$index] }
        "--actor-id" { $actorId = $args[++$index] }
        "--actor-context" { $actorContext = $args[++$index] }
        "--session-id" { $sessionId = $args[++$index] }
        { $_ -in @("held", "next", "focus", "inspect") } { $operation = $args[$index] }
    }
}

if (-not $projectFile -or -not $engramHome -or -not $actorId -or -not $sessionId) {
    exit 2
}
if ($actorId -ne "dev/codex" -or $actorContext -ne "agent=codex;model=test;reasoning=high" -or $sessionId -ne "fixture-session") {
    exit 3
}
if ($env:ENGRAM_HOME -ne $engramHome -or $env:ENGRAM_ACTOR_ID -ne $actorId -or $env:ENGRAM_ACTOR_CONTEXT -ne $actorContext -or $env:ENGRAM_SESSION_ID -ne $sessionId) {
    exit 7
}
$mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
Add-Content -LiteralPath (Join-Path $engramHome "work-read-phases") -Value $operation
$lockRetryMarker = Join-Path $engramHome "work-lock-retried"
# The binding reader reads `core held` only; focus-based reads select focus.
if ($operation -ne "held") {
    exit 5
}
if ($mode -eq "held-missing") {
    # An Engram build that predates the command.
    [Console]::Error.WriteLine("error: unrecognized subcommand 'held'")
    exit 2
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
$fixtureBinding = '{"root_execution_id":"root-fixture","work_id":"work-fixture","run_id":"run-fixture","work_revision":17,"claim_id":"claim-fixture","claim_fence":23}'
$newerBinding = '{"root_execution_id":"root-newer","work_id":"work-newer","run_id":"run-newer","work_revision":4,"claim_id":"claim-newer","claim_fence":3}'
switch ($mode) {
    "unbound-null" {
        [Console]::Out.WriteLine('{"items":[{"work_id":"work-fixture","focused":true,"control_binding":null}],"focused_work_id":"work-fixture","total":1,"omitted":0}')
    }
    "unbound-absent" {
        [Console]::Out.WriteLine('{"items":[{"work_id":"work-fixture","focused":true}],"focused_work_id":"work-fixture","total":1,"omitted":0}')
    }
    "no-claims" {
        [Console]::Out.WriteLine('{"items":[],"focused_work_id":null,"total":0,"omitted":0}')
    }
    "malformed-binding" {
        [Console]::Out.WriteLine('{"items":[{"work_id":"work-fixture","focused":true,"control_binding":{"work_id":"work-fixture"}}],"focused_work_id":"work-fixture","total":1,"omitted":0}')
    }
    "malformed-output" {
        [Console]::Out.WriteLine('{"held":[]}')
    }
    default {
        [Console]::Out.WriteLine('{"items":[{"work_id":"work-newer","short_ref":"w-newer","claim_id":"claim-newer","claim_fence":3,"claimed_at":"2026-09-24T17:00:00Z","expires_at":"2026-09-24T18:00:00Z","focused":false,"control_binding":' + $newerBinding + '},{"work_id":"work-fixture","short_ref":"w-fixture","claim_id":"claim-fixture","claim_fence":23,"claimed_at":"2026-09-24T16:00:00Z","expires_at":"2026-09-24T18:00:00Z","focused":true,"control_binding":' + $fixtureBinding + '}],"focused_work_id":"work-fixture","total":2,"omitted":0}')
    }
}
exit 0
