$ErrorActionPreference = "Stop"

$projectFile = $null
for ($index = 0; $index -lt $args.Count - 1; $index += 1) {
    if ($args[$index] -eq "--project-file") {
        $projectFile = $args[$index + 1]
        break
    }
}
if (-not $projectFile) {
    exit 2
}

if ($args -contains "doctor") {
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    switch ($mode) {
        "fixture-doctor-turn-gated" {
            [Console]::Out.WriteLine("Control policy schema=1 epoch=1 required=TurnGated supported=[Observe, Communicate]")
            exit 0
        }
        "fixture-doctor-action-gated" {
            [Console]::Out.WriteLine("Control policy schema=1 epoch=1 required=ActionGated supported=[Observe, Communicate, MutateLocal]")
            exit 0
        }
        "fixture-doctor-missing-required" {
            [Console]::Out.WriteLine("Engram store is healthy")
            exit 0
        }
        default {
            [Console]::Out.WriteLine("Control policy schema=1 epoch=1 required=Advisory supported=[Observe, Communicate]")
            exit 0
        }
    }
}

$routingToken = "fixture-token"
$issuedGrant = $null
$begunGrant = $null
$grantCounter = 0
$bindCount = 0
$seenBeginIntents = @{}
$seenBeginDecisions = @{}
$seenBeginCodes = @{}
$seenEvaluateIntents = @{}
$seenEvaluateGrants = @{}

function Write-Result($result) {
    [Console]::Out.WriteLine((@{ status = "ok"; result = $result } | ConvertTo-Json -Compress -Depth 10))
    [Console]::Out.Flush()
}

function Write-ControlError([string] $code, [string] $message) {
    [Console]::Out.WriteLine((@{
        status = "error"
        error = @{ code = $code; message = $message }
    } | ConvertTo-Json -Compress -Depth 10))
    [Console]::Out.Flush()
}

function Write-EvaluationRefusal([string] $code) {
    Write-Result @{
        decision = "refuse"
        directive = @{
            directive_id = "directive-$code"
            code = $code
            target = "host"
            satisfaction = "checkpoint the open turn"
        }
    }
}

[Console]::Out.WriteLine("termal-engram-control-fixture-ready")
[Console]::Out.Flush()

while ($null -ne ($line = [Console]::In.ReadLine())) {
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    if ($mode -eq "fixture-eof") {
        exit 0
    }
    if ($mode -eq "fixture-hang") {
        Start-Sleep -Seconds 30
        continue
    }
    if ($mode -eq "fixture-malformed") {
        [Console]::Out.WriteLine('{"status":')
        [Console]::Out.Flush()
        continue
    }
    $request = $line | ConvertFrom-Json
    if (-not $mode.StartsWith("fixture-stateful")) {
        if ($request.operation -ne "session_bind") {
            Write-ControlError "invalid_request" "fixture only accepts session_bind"
            continue
        }
        Write-Result @{ routing_token = $routingToken; status = @{ phase = "ready" } }
        continue
    }

    if ($request.operation -ne "session_bind" -and $request.routing_token -ne $routingToken) {
        Write-ControlError "control_session_token_mismatch" "routing token does not match"
        continue
    }

    switch ($request.operation) {
        "session_bind" {
            if ($begunGrant) {
                Write-ControlError "invalid_control_session" "a begun grant must be checkpointed before bind"
                continue
            }
            # The real control plane expires an issued-but-never-begun grant
            # when the host establishes a fresh binding.
            $issuedGrant = $null
            $bindCount += 1
            $routingToken = "fixture-token-$bindCount"
            Write-Result @{ routing_token = $routingToken; status = @{ phase = "sync_required" } }
        }
        "session_status" {
            $status = @{ phase = if ($begunGrant -or $issuedGrant) { "turn_open" } else { "ready" } }
            if ($begunGrant) {
                $status.open_grant_id = $begunGrant
            } elseif ($issuedGrant) {
                $status.open_grant_id = $issuedGrant
            }
            Write-Result $status
        }
        "turn_evaluate" {
            $key = [string] $request.idempotency_key
            $fingerprint = [string] $request.intent_fingerprint
            if ($seenEvaluateIntents.ContainsKey($key)) {
                if ($seenEvaluateIntents[$key] -ne $fingerprint) {
                    Write-ControlError "turn_idempotency_conflict" "idempotency key was reused with a different intent fingerprint"
                    continue
                }
                Write-Result @{
                    decision = "grant"
                    grant = @{ grant_id = $seenEvaluateGrants[$key] }
                }
                continue
            }
            if ($issuedGrant -or $begunGrant) {
                Write-EvaluationRefusal "turn_already_open"
                continue
            }
            $grantCounter += 1
            $issuedGrant = "fixture-grant-$grantCounter"
            $seenEvaluateIntents[$key] = $fingerprint
            $seenEvaluateGrants[$key] = $issuedGrant
            Write-Result @{
                decision = "grant"
                grant = @{ grant_id = $issuedGrant }
            }
        }
        "turn_begin" {
            $key = [string] $request.idempotency_key
            $grant = [string] $request.grant_id
            if ($seenBeginIntents.ContainsKey($key) -and $seenBeginIntents[$key] -ne $grant) {
                Write-ControlError "control_operation_idempotency_conflict" "idempotency key was reused with a different grant"
                continue
            }
            if ($seenBeginIntents.ContainsKey($key)) {
                if ($seenBeginDecisions[$key] -eq "begin") {
                    Write-Result @{ decision = "begin"; receipt = @{ grant_id = $grant } }
                } else {
                    Write-Result @{ decision = "refuse"; code = $seenBeginCodes[$key] }
                }
                continue
            }
            if ($grant -ne $issuedGrant) {
                Write-ControlError "grant_scope_mismatch" "grant is not the currently issued grant"
                continue
            }
            $seenBeginIntents[$key] = $grant
            if ($mode -eq "fixture-stateful-stale-begin" -and $grantCounter -eq 1) {
                $issuedGrant = $null
                $seenBeginDecisions[$key] = "refuse"
                $seenBeginCodes[$key] = "policy_epoch_changed"
                Write-Result @{ decision = "refuse"; code = "policy_epoch_changed" }
                continue
            }
            if ($mode -eq "fixture-stateful-lifecycle-hold-begin" -and $grantCounter -eq 1) {
                # lifecycle_hold is non-expiring: Engram retains the issued
                # grant until a fresh bind explicitly expires it.
                $seenBeginDecisions[$key] = "refuse"
                $seenBeginCodes[$key] = "lifecycle_hold"
                Write-Result @{ decision = "refuse"; code = "lifecycle_hold" }
                continue
            }
            if ($mode -eq "fixture-stateful-delivery-invalid-begin" -and $grantCounter -eq 1) {
                # delivery_invalid is also non-expiring in the real control
                # plane and therefore must not silently clear issued state.
                $seenBeginDecisions[$key] = "refuse"
                $seenBeginCodes[$key] = "delivery_invalid"
                Write-Result @{ decision = "refuse"; code = "delivery_invalid" }
                continue
            }
            $issuedGrant = $null
            $begunGrant = $grant
            $seenBeginDecisions[$key] = "begin"
            Write-Result @{ decision = "begin"; receipt = @{ grant_id = $grant } }
        }
        "turn_checkpoint" {
            $grant = [string] $request.grant_id
            if ($issuedGrant -and $grant -eq $issuedGrant) {
                Write-Result @{ decision = "refuse"; code = "grant_scope_mismatch" }
                continue
            }
            if (-not $begunGrant -or $grant -ne $begunGrant) {
                Write-ControlError "grant_scope_mismatch" "only a begun grant can be checkpointed"
                continue
            }
            $issuedGrant = $null
            $begunGrant = $null
            Write-Result @{
                decision = "checkpointed"
                receipt = @{ grant_id = $grant; cursor = 1; confirmed_cursor = 1 }
            }
        }
        default {
            Write-ControlError "invalid_request" "unsupported fixture operation"
        }
    }
}
