$ErrorActionPreference = "Stop"

$projectFile = $null
$engramHome = $null
for ($index = 0; $index -lt $args.Count - 1; $index += 1) {
    if ($args[$index] -eq "--project-file") {
        $projectFile = $args[$index + 1]
    }
    if ($args[$index] -eq "--home") {
        $engramHome = $args[$index + 1]
    }
}
if (-not $projectFile -or -not $engramHome) {
    exit 2
}

if (($args -contains "authority") -and ($args -contains "show")) {
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    $result = [ordered]@{
        installed = $true
        subject_actor_id = "termal"
        issued_by = "test"
        valid_from = "2020-01-01T00:00:00Z"
        valid_until = "2099-01-01T00:00:00Z"
        revoked_at = $null
        operations = @("root_create", "plan", "claim", "dispose", "root_complete", "completion_drain")
        scope = @{ kind = "project" }
    }
    switch ($mode) {
        "fixture-authority-unknown" { $result.installed = $false; $result.subject_actor_id = $null; $result.valid_from = $null; $result.valid_until = $null; $result.operations = $null; $result.scope = $null }
        "fixture-authority-revoked" { $result.revoked_at = "2026-08-30T22:26:46.8Z" }
        "fixture-authority-expired" { $result.valid_until = "2020-01-02T00:00:00Z" }
        "fixture-authority-future" { $result.valid_from = "2098-01-01T00:00:00Z" }
        "fixture-authority-subject-mismatch" { $result.subject_actor_id = "another-actor" }
    }
    [Console]::Out.WriteLine(($result | ConvertTo-Json -Compress -Depth 10))
    exit 0
}

if (($args -contains "authority") -and ($args -contains "revoke")) {
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    $args | ConvertTo-Json -Compress | Set-Content -LiteralPath (Join-Path $engramHome "engram-authority-revoke-args.json")
    if ($mode -eq "fixture-authority-revoke-fail") {
        [Console]::Error.WriteLine("scripted authority revoke failure")
        exit 9
    }
    if ($mode -eq "fixture-authority-revoke-fail-once") {
        $failOnceMarker = Join-Path $engramHome "engram-authority-revoke-failed-once"
        if (-not (Test-Path -LiteralPath $failOnceMarker)) {
            Set-Content -LiteralPath $failOnceMarker -Value "failed"
            [Console]::Error.WriteLine("scripted one-time authority revoke failure")
            exit 9
        }
    }
    [Console]::Out.WriteLine("fixture-revocation-hash")
    exit 0
}

if ($args -contains "doctor") {
    $mode = (Get-Content -LiteralPath $projectFile -Raw).Trim()
    $requiredAssurance = "turn_gated"
    $control = @{
        required_assurance = $requiredAssurance
    }
    switch ($mode) {
        "fixture-doctor-advisory" { $control.required_assurance = "advisory" }
        "fixture-doctor-action-gated" {
            $control.required_assurance = "action_gated"
        }
        "fixture-doctor-missing-required" {
            $control = $null
        }
    }
    $database = [System.IO.Path]::GetFullPath((Join-Path $engramHome "fixture-engram.db"))
    [Console]::Out.WriteLine((@{
        healthy = $true
        control = $control
        database = $database
        project_id = $mode
    } | ConvertTo-Json -Compress -Depth 10))
    exit 0
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
$seenEvaluateDecisions = @{}
$seenEvaluateCodes = @{}
$seenBindIntents = @{}
$seenBindTokens = @{}
$seenCheckpointIntents = @{}
$seenCheckpointDecisions = @{}
$seenCheckpointCodes = @{}
$knownGrants = @{}

function Get-RequestIntent($request) {
    $intent = [ordered]@{}
    foreach ($property in $request.PSObject.Properties) {
        if ($property.Name -ne "routing_token" -and $property.Name -ne "idempotency_key") {
            $intent[$property.Name] = $property.Value
        }
    }
    return ($intent | ConvertTo-Json -Compress -Depth 10)
}

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
    if ($mode.StartsWith("fixture-tree-")) {
        $descendantPath = Join-Path $engramHome "engram-descendant.ps1"
        if ($mode -eq "fixture-tree-eof") {
            Start-Process -FilePath "powershell.exe" -ArgumentList @(
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-File",
                $descendantPath
            ) -WindowStyle Hidden
        } else {
            $processInfo = [System.Diagnostics.ProcessStartInfo]::new()
            $processInfo.FileName = "powershell.exe"
            $processInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -File `"$descendantPath`""
            $processInfo.UseShellExecute = $false
            $processInfo.CreateNoWindow = $true
            [System.Diagnostics.Process]::Start($processInfo) | Out-Null
        }
        $spawnedPath = Join-Path $engramHome "engram-descendant-spawned"
        $spawnDeadline = [DateTime]::UtcNow.AddSeconds(10)
        while (-not (Test-Path -LiteralPath $spawnedPath) -and [DateTime]::UtcNow -lt $spawnDeadline) {
            Start-Sleep -Milliseconds 10
        }
        if (-not (Test-Path -LiteralPath $spawnedPath)) {
            exit 3
        }
        if ($mode -eq "fixture-tree-eof") {
            $releasePath = Join-Path $engramHome "engram-eof-release"
            $releaseDeadline = [DateTime]::UtcNow.AddSeconds(10)
            while (-not (Test-Path -LiteralPath $releasePath) -and [DateTime]::UtcNow -lt $releaseDeadline) {
                Start-Sleep -Milliseconds 10
            }
            if (-not (Test-Path -LiteralPath $releasePath)) {
                exit 4
            }
            exit 0
        }
        if ($mode -eq "fixture-tree-reply") {
            Write-Result @{ routing_token = $routingToken; status = @{ phase = "ready" } }
            continue
        }
        Start-Sleep -Seconds 30
        continue
    }
    if ($mode -eq "fixture-malformed") {
        [Console]::Out.WriteLine('{"status":')
        [Console]::Out.Flush()
        continue
    }
    $request = $line | ConvertFrom-Json
    if ($request.operation -eq "session_bind" -and $request.assurance -ne "turn_gated") {
        Write-ControlError "invalid_assurance" "fixture requires turn_gated assurance"
        continue
    }
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
            $key = [string] $request.idempotency_key
            $intent = Get-RequestIntent $request
            if ($seenBindIntents.ContainsKey($key)) {
                if ($seenBindIntents[$key] -ne $intent) {
                    Write-ControlError "control_session_bind_conflict" "session_bind idempotency key was reused for a different intent"
                } else {
                    Write-Result @{ routing_token = $seenBindTokens[$key]; status = @{ phase = "sync_required" } }
                }
                continue
            }
            if ($begunGrant) {
                Write-ControlError "invalid_control_session" "a begun grant must be checkpointed before bind"
                continue
            }
            # The real control plane expires an issued-but-never-begun grant
            # when the host establishes a fresh binding.
            $issuedGrant = $null
            $bindCount += 1
            $routingToken = "fixture-token-$bindCount"
            $seenBindIntents[$key] = $intent
            $seenBindTokens[$key] = $routingToken
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
                if ($seenEvaluateDecisions[$key] -eq "refuse") {
                    Write-EvaluationRefusal $seenEvaluateCodes[$key]
                } else {
                    Write-Result @{
                        decision = "grant"
                        grant = @{ grant_id = $seenEvaluateGrants[$key] }
                    }
                }
                continue
            }
            if ($issuedGrant -or $begunGrant) {
                $seenEvaluateIntents[$key] = $fingerprint
                $seenEvaluateDecisions[$key] = "refuse"
                $seenEvaluateCodes[$key] = "turn_already_open"
                Write-EvaluationRefusal "turn_already_open"
                continue
            }
            $grantCounter += 1
            $issuedGrant = "fixture-grant-$grantCounter"
            $seenEvaluateIntents[$key] = $fingerprint
            $seenEvaluateGrants[$key] = $issuedGrant
            $seenEvaluateDecisions[$key] = "grant"
            $knownGrants[$issuedGrant] = $true
            Write-Result @{
                decision = "grant"
                grant = @{ grant_id = $issuedGrant }
            }
        }
        "turn_begin" {
            $key = [string] $request.idempotency_key
            $grant = [string] $request.grant_id
            $deliveryTokens = @($request.delivery_tokens) | ConvertTo-Json -Compress -Depth 10
            $intent = "$grant|$deliveryTokens"
            if ($seenBeginIntents.ContainsKey($key) -and $seenBeginIntents[$key] -ne $intent) {
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
            if (-not $knownGrants.ContainsKey($grant)) {
                Write-ControlError "turn_grant_not_found" "grant does not exist"
                continue
            }
            if ($grant -ne $issuedGrant) {
                $seenBeginIntents[$key] = $intent
                $seenBeginDecisions[$key] = "refuse"
                $seenBeginCodes[$key] = "grant_scope_mismatch"
                Write-Result @{ decision = "refuse"; code = "grant_scope_mismatch" }
                continue
            }
            $seenBeginIntents[$key] = $intent
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
            $key = [string] $request.idempotency_key
            $intent = Get-RequestIntent $request
            if ($seenCheckpointIntents.ContainsKey($key)) {
                if ($seenCheckpointIntents[$key] -ne $intent) {
                    Write-ControlError "control_operation_idempotency_conflict" "turn_checkpoint idempotency key was reused for a different intent"
                } elseif ($seenCheckpointDecisions[$key] -eq "refuse") {
                    Write-Result @{ decision = "refuse"; code = $seenCheckpointCodes[$key] }
                } else {
                    Write-Result @{
                        decision = "checkpointed"
                        receipt = @{ grant_id = $grant; cursor = 1; confirmed_cursor = 1 }
                    }
                }
                continue
            }
            if (-not $knownGrants.ContainsKey($grant)) {
                Write-ControlError "turn_grant_not_found" "grant does not exist"
                continue
            }
            if (-not $begunGrant -or $grant -ne $begunGrant) {
                # Checkpointing the still-issued grant refuses grant_not_begun;
                # checkpointing any other grant is a scope mismatch.
                $code = if ($issuedGrant -and $grant -eq $issuedGrant) {
                    "grant_not_begun"
                } else {
                    "grant_scope_mismatch"
                }
                $seenCheckpointIntents[$key] = $intent
                $seenCheckpointDecisions[$key] = "refuse"
                $seenCheckpointCodes[$key] = $code
                Write-Result @{ decision = "refuse"; code = $code }
                continue
            }
            $issuedGrant = $null
            $begunGrant = $null
            $seenCheckpointIntents[$key] = $intent
            $seenCheckpointDecisions[$key] = "checkpointed"
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
