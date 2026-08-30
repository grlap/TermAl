#!/bin/sh
set -eu

project_file=""
engram_home=""
is_doctor=0
original_args=$*
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--project-file" ]; then
    project_file="$2"
    shift 2
    continue
  fi
  if [ "$1" = "--home" ]; then
    engram_home="$2"
    shift 2
    continue
  fi
  if [ "$1" = "doctor" ]; then
    is_doctor=1
  fi
  shift
done
[ -n "$project_file" ] || exit 2
[ -n "$engram_home" ] || exit 2

case " $original_args " in
  *" authority revoke "*)
    printf '%s\n' "$original_args" > "$engram_home/engram-authority-revoke-args.txt"
    mode=$(tr -d '\r\n' < "$project_file")
    if [ "$mode" = "fixture-authority-revoke-fail" ]; then
      printf '%s\n' 'scripted authority revoke failure' >&2
      exit 9
    fi
    if [ "$mode" = "fixture-authority-revoke-fail-once" ]; then
      fail_once_marker="$engram_home/engram-authority-revoke-failed-once"
      if [ ! -e "$fail_once_marker" ]; then
        : > "$fail_once_marker"
        printf '%s\n' 'scripted one-time authority revoke failure' >&2
        exit 9
      fi
    fi
    printf '%s\n' 'fixture-revocation-hash'
    exit 0
    ;;
esac

if [ "$is_doctor" -eq 1 ]; then
  mode=$(tr -d '\r\n' < "$project_file")
  case "$mode" in
    fixture-doctor-turn-gated)
      printf '%s\n' 'Control policy schema=1 epoch=1 required=TurnGated supported=[Observe, Communicate]'
      ;;
    fixture-doctor-action-gated)
      printf '%s\n' 'Control policy schema=1 epoch=1 required=ActionGated supported=[Observe, Communicate, MutateLocal]'
      ;;
    fixture-doctor-missing-required)
      printf '%s\n' 'Engram store is healthy'
      ;;
    *)
      printf '%s\n' 'Control policy schema=1 epoch=1 required=Advisory supported=[Observe, Communicate]'
      ;;
  esac
  exit 0
fi

routing_token="fixture-token"
issued_grant=""
begun_grant=""
grant_counter=0
bind_count=0
seen_begin_pairs=""
seen_evaluate_rows=""
seen_evaluate_refusal_rows=""
seen_bind_rows=""
seen_checkpoint_rows=""
known_grants=""

json_field() {
  printf '%s' "$1" | sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p"
}

write_error() {
  printf '{"status":"error","error":{"code":"%s","message":"%s"}}\n' "$1" "$2"
}

write_evaluation_refusal() {
  refusal_code=$1
  printf '{"status":"ok","result":{"decision":"refuse","directive":{"directive_id":"directive-%s","code":"%s","target":"host","satisfaction":"checkpoint the open turn"}}}\n' "$refusal_code" "$refusal_code"
}

request_intent() {
  printf '%s' "$1" | sed 's/"routing_token":"[^"]*",//; s/,"idempotency_key":"[^"]*"}$/}/'
}

grant_is_known() {
  lookup_grant=$1
  while IFS= read -r stored_grant; do
    [ "$stored_grant" = "$lookup_grant" ] && return 0
  done <<EOF
$known_grants
EOF
  return 1
}

lookup_begin_row() {
  lookup_key=$1
  while IFS='|' read -r stored_key stored_grant stored_decision stored_code; do
    if [ "$stored_key" = "$lookup_key" ]; then
      printf '%s|%s|%s' "$stored_grant" "$stored_decision" "$stored_code"
      return
    fi
  done <<EOF
$seen_begin_pairs
EOF
}

lookup_evaluate_row() {
  lookup_key=$1
  while IFS='|' read -r stored_key stored_fingerprint stored_grant; do
    if [ "$stored_key" = "$lookup_key" ]; then
      printf '%s|%s' "$stored_fingerprint" "$stored_grant"
      return
    fi
  done <<EOF
$seen_evaluate_rows
EOF
}

lookup_evaluate_refusal() {
  lookup_key=$1
  while IFS='|' read -r stored_key stored_fingerprint stored_code; do
    if [ "$stored_key" = "$lookup_key" ]; then
      printf '%s|%s' "$stored_fingerprint" "$stored_code"
      return
    fi
  done <<EOF
$seen_evaluate_refusal_rows
EOF
}

lookup_bind_row() {
  lookup_key=$1
  while IFS='|' read -r stored_key stored_intent stored_token; do
    if [ "$stored_key" = "$lookup_key" ]; then
      printf '%s|%s' "$stored_intent" "$stored_token"
      return
    fi
  done <<EOF
$seen_bind_rows
EOF
}

lookup_checkpoint_row() {
  lookup_key=$1
  while IFS='|' read -r stored_key stored_intent stored_decision stored_code stored_grant; do
    if [ "$stored_key" = "$lookup_key" ]; then
      printf '%s|%s|%s|%s' "$stored_intent" "$stored_decision" "$stored_code" "$stored_grant"
      return
    fi
  done <<EOF
$seen_checkpoint_rows
EOF
}

printf '%s\n' 'termal-engram-control-fixture-ready'

while IFS= read -r line; do
  mode=$(tr -d '\r\n' < "$project_file")
  case "$mode" in
    fixture-eof)
      exit 0
      ;;
    fixture-hang)
      sleep 30
      ;;
    fixture-tree-hang)
      sh "$engram_home/engram-descendant.sh" &
      while [ ! -e "$engram_home/engram-descendant-spawned" ]; do sleep 0.01; done
      sleep 30
      ;;
    fixture-tree-eof)
      sh "$engram_home/engram-descendant.sh" >/dev/null &
      while [ ! -e "$engram_home/engram-descendant-spawned" ]; do sleep 0.01; done
      release_attempts=0
      while [ ! -e "$engram_home/engram-eof-release" ]; do
        release_attempts=$((release_attempts + 1))
        if [ "$release_attempts" -ge 1000 ]; then
          exit 4
        fi
        sleep 0.01
      done
      exit 0
      ;;
    fixture-tree-reply)
      sh "$engram_home/engram-descendant.sh" &
      while [ ! -e "$engram_home/engram-descendant-spawned" ]; do sleep 0.01; done
      printf '%s\n' '{"status":"ok","result":{"routing_token":"fixture-token","status":{"phase":"ready"}}}'
      ;;
    fixture-malformed)
      printf '%s\n' '{"status":'
      ;;
    fixture-stateful*)
      operation=$(json_field "$line" operation)
      request_token=$(json_field "$line" routing_token)
      if [ "$operation" != "session_bind" ] && [ "$request_token" != "$routing_token" ]; then
        write_error control_session_token_mismatch 'routing token does not match'
        continue
      fi
      case "$operation" in
        session_bind)
          key=$(json_field "$line" idempotency_key)
          intent=$(request_intent "$line")
          existing_bind=$(lookup_bind_row "$key")
          if [ -n "$existing_bind" ]; then
            existing_intent=${existing_bind%%|*}
            existing_token=${existing_bind#*|}
            if [ "$existing_intent" != "$intent" ]; then
              write_error control_session_bind_conflict 'session_bind idempotency key was reused for a different intent'
            else
              printf '{"status":"ok","result":{"routing_token":"%s","status":{"phase":"sync_required"}}}\n' "$existing_token"
            fi
            continue
          fi
          if [ -n "$begun_grant" ]; then
            write_error invalid_control_session 'a begun grant must be checkpointed before bind'
            continue
          fi
          issued_grant=""
          bind_count=$((bind_count + 1))
          routing_token="fixture-token-$bind_count"
          seen_bind_rows="${seen_bind_rows}
$key|$intent|$routing_token"
          printf '{"status":"ok","result":{"routing_token":"%s","status":{"phase":"sync_required"}}}\n' "$routing_token"
          ;;
        session_status)
          if [ -n "$begun_grant" ]; then
            open_grant=$begun_grant
          else
            open_grant=$issued_grant
          fi
          if [ -n "$open_grant" ]; then
            printf '{"status":"ok","result":{"phase":"turn_open","open_grant_id":"%s"}}\n' "$open_grant"
          else
            printf '%s\n' '{"status":"ok","result":{"phase":"ready"}}'
          fi
          ;;
        turn_evaluate)
          key=$(json_field "$line" idempotency_key)
          fingerprint=$(json_field "$line" intent_fingerprint)
          existing_refusal=$(lookup_evaluate_refusal "$key")
          if [ -n "$existing_refusal" ]; then
            existing_fingerprint=${existing_refusal%%|*}
            existing_code=${existing_refusal#*|}
            if [ "$existing_fingerprint" != "$fingerprint" ]; then
              write_error turn_idempotency_conflict 'idempotency key was reused with a different intent fingerprint'
            else
              write_evaluation_refusal "$existing_code"
            fi
            continue
          fi
          existing_evaluate=$(lookup_evaluate_row "$key")
          if [ -n "$existing_evaluate" ]; then
            existing_fingerprint=${existing_evaluate%%|*}
            existing_grant=${existing_evaluate#*|}
            if [ "$existing_fingerprint" != "$fingerprint" ]; then
              write_error turn_idempotency_conflict 'idempotency key was reused with a different intent fingerprint'
              continue
            fi
            printf '{"status":"ok","result":{"decision":"grant","grant":{"grant_id":"%s"}}}\n' "$existing_grant"
            continue
          fi
          if [ -n "$issued_grant" ] || [ -n "$begun_grant" ]; then
            seen_evaluate_refusal_rows="${seen_evaluate_refusal_rows}
$key|$fingerprint|turn_already_open"
            write_evaluation_refusal turn_already_open
            continue
          fi
          grant_counter=$((grant_counter + 1))
          issued_grant="fixture-grant-$grant_counter"
          seen_evaluate_rows="${seen_evaluate_rows}
$key|$fingerprint|$issued_grant"
          known_grants="${known_grants}
$issued_grant"
          printf '{"status":"ok","result":{"decision":"grant","grant":{"grant_id":"%s"}}}\n' "$issued_grant"
          ;;
        turn_begin)
          key=$(json_field "$line" idempotency_key)
          grant=$(json_field "$line" grant_id)
          intent=$(request_intent "$line")
          existing_row=$(lookup_begin_row "$key")
          existing_intent=${existing_row%%|*}
          if [ -n "$existing_intent" ] && [ "$intent" != "$existing_intent" ]; then
            write_error control_operation_idempotency_conflict 'idempotency key was reused with a different grant'
            continue
          fi
          if [ -n "$existing_intent" ]; then
            existing_remainder=${existing_row#*|}
            existing_decision=${existing_remainder%%|*}
            existing_code=${existing_remainder#*|}
            if [ "$existing_decision" = "begin" ]; then
              printf '{"status":"ok","result":{"decision":"begin","receipt":{"grant_id":"%s"}}}\n' "$grant"
            else
              printf '{"status":"ok","result":{"decision":"refuse","code":"%s"}}\n' "$existing_code"
            fi
            continue
          fi
          if ! grant_is_known "$grant"; then
            write_error turn_grant_not_found 'grant does not exist'
            continue
          fi
          if [ "$grant" != "$issued_grant" ]; then
            seen_begin_pairs="${seen_begin_pairs}
$key|$intent|refuse|grant_scope_mismatch"
            printf '%s\n' '{"status":"ok","result":{"decision":"refuse","code":"grant_scope_mismatch"}}'
            continue
          fi
          if [ "$mode" = "fixture-stateful-stale-begin" ] && [ "$grant_counter" -eq 1 ]; then
            issued_grant=""
            seen_begin_pairs="${seen_begin_pairs}
$key|$intent|refuse|policy_epoch_changed"
            printf '%s\n' '{"status":"ok","result":{"decision":"refuse","code":"policy_epoch_changed"}}'
            continue
          fi
          if [ "$mode" = "fixture-stateful-lifecycle-hold-begin" ] && [ "$grant_counter" -eq 1 ]; then
            # lifecycle_hold is non-expiring: retain the issued grant until a
            # fresh bind explicitly expires it.
            seen_begin_pairs="${seen_begin_pairs}
$key|$intent|refuse|lifecycle_hold"
            printf '%s\n' '{"status":"ok","result":{"decision":"refuse","code":"lifecycle_hold"}}'
            continue
          fi
          if [ "$mode" = "fixture-stateful-delivery-invalid-begin" ] && [ "$grant_counter" -eq 1 ]; then
            # delivery_invalid is non-expiring in the real control plane too.
            seen_begin_pairs="${seen_begin_pairs}
$key|$intent|refuse|delivery_invalid"
            printf '%s\n' '{"status":"ok","result":{"decision":"refuse","code":"delivery_invalid"}}'
            continue
          fi
          issued_grant=""
          begun_grant=$grant
          seen_begin_pairs="${seen_begin_pairs}
$key|$intent|begin|"
          printf '{"status":"ok","result":{"decision":"begin","receipt":{"grant_id":"%s"}}}\n' "$grant"
          ;;
        turn_checkpoint)
          grant=$(json_field "$line" grant_id)
          key=$(json_field "$line" idempotency_key)
          intent=$(request_intent "$line")
          existing_checkpoint=$(lookup_checkpoint_row "$key")
          if [ -n "$existing_checkpoint" ]; then
            existing_intent=${existing_checkpoint%%|*}
            existing_remainder=${existing_checkpoint#*|}
            existing_decision=${existing_remainder%%|*}
            existing_remainder=${existing_remainder#*|}
            existing_code=${existing_remainder%%|*}
            existing_grant=${existing_remainder#*|}
            if [ "$existing_intent" != "$intent" ]; then
              write_error control_operation_idempotency_conflict 'turn_checkpoint idempotency key was reused for a different intent'
            elif [ "$existing_decision" = "refuse" ]; then
              printf '{"status":"ok","result":{"decision":"refuse","code":"%s"}}\n' "$existing_code"
            else
              printf '{"status":"ok","result":{"decision":"checkpointed","receipt":{"grant_id":"%s","cursor":1,"confirmed_cursor":1}}}\n' "$existing_grant"
            fi
            continue
          fi
          if ! grant_is_known "$grant"; then
            write_error turn_grant_not_found 'grant does not exist'
            continue
          fi
          if [ -z "$begun_grant" ] || [ "$grant" != "$begun_grant" ]; then
            # Checkpointing the still-issued grant refuses grant_not_begun;
            # checkpointing any other grant is a scope mismatch.
            if [ -n "$issued_grant" ] && [ "$grant" = "$issued_grant" ]; then
              refusal_code="grant_not_begun"
            else
              refusal_code="grant_scope_mismatch"
            fi
            seen_checkpoint_rows="${seen_checkpoint_rows}
$key|$intent|refuse|$refusal_code|$grant"
            printf '{"status":"ok","result":{"decision":"refuse","code":"%s"}}\n' "$refusal_code"
            continue
          fi
          issued_grant=""
          begun_grant=""
          seen_checkpoint_rows="${seen_checkpoint_rows}
$key|$intent|checkpointed||$grant"
          printf '{"status":"ok","result":{"decision":"checkpointed","receipt":{"grant_id":"%s","cursor":1,"confirmed_cursor":1}}}\n' "$grant"
          ;;
        *)
          write_error invalid_request 'unsupported fixture operation'
          ;;
      esac
      ;;
    *)
      case "$line" in
        *'"operation":"session_bind"'*)
          printf '%s\n' '{"status":"ok","result":{"routing_token":"fixture-token","status":{"phase":"ready"}}}'
          ;;
        *)
          printf '%s\n' '{"status":"error","error":{"code":"invalid_request","message":"fixture only accepts session_bind"}}'
          ;;
      esac
      ;;
  esac
done
