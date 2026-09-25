#!/bin/sh
set -eu

project_file=""
engram_home=""
actor_id=""
actor_context=""
session_id=""
operation=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --project-file) project_file=$2; shift 2 ;;
    --home) engram_home=$2; shift 2 ;;
    --actor-id) actor_id=$2; shift 2 ;;
    --actor-context) actor_context=$2; shift 2 ;;
    --session-id) session_id=$2; shift 2 ;;
    work) shift ;;
    held|next|focus|inspect) operation=$1; shift ;;
    *) shift ;;
  esac
done

[ -n "$project_file" ] || exit 2
[ -n "$engram_home" ] || exit 2
[ "$actor_id" = "dev/codex" ] || exit 3
[ "$actor_context" = "agent=codex;model=test;reasoning=high" ] || exit 3
[ "$session_id" = "fixture-session" ] || exit 3
[ "${ENGRAM_HOME:-}" = "$engram_home" ] || exit 7
[ "${ENGRAM_ACTOR_ID:-}" = "$actor_id" ] || exit 7
[ "${ENGRAM_ACTOR_CONTEXT:-}" = "$actor_context" ] || exit 7
[ "${ENGRAM_SESSION_ID:-}" = "$session_id" ] || exit 7
mode=$(tr -d '\r\n' < "$project_file")
printf '%s\n' "$operation" >> "$engram_home/work-read-phases"
lock_retry_marker="$engram_home/work-lock-retried"
# The binding reader reads `core held` only; focus-based reads select focus.
[ "$operation" = held ] || exit 5
if [ "$mode" = held-missing ]; then
  # An Engram build that predates the command.
  printf '%s\n' "error: unrecognized subcommand 'held'" >&2
  exit 2
fi
if [ "$mode" = read-error ]; then
  printf '%s\n' 'database is locked' >&2
  exit 6
fi
if [ "$mode" = read-error-once ] && [ ! -e "$lock_retry_marker" ]; then
  printf retry > "$lock_retry_marker"
  printf '%s\n' 'database is locked' >&2
  exit 6
fi
fixture_binding='{"root_execution_id":"root-fixture","work_id":"work-fixture","run_id":"run-fixture","work_revision":17,"claim_id":"claim-fixture","claim_fence":23}'
newer_binding='{"root_execution_id":"root-newer","work_id":"work-newer","run_id":"run-newer","work_revision":4,"claim_id":"claim-newer","claim_fence":3}'
case "$mode" in
  unbound-null)
    printf '%s\n' '{"items":[{"work_id":"work-fixture","focused":true,"control_binding":null}],"focused_work_id":"work-fixture","total":1,"omitted":0}'
    ;;
  unbound-absent)
    printf '%s\n' '{"items":[{"work_id":"work-fixture","focused":true}],"focused_work_id":"work-fixture","total":1,"omitted":0}'
    ;;
  no-claims)
    printf '%s\n' '{"items":[],"focused_work_id":null,"total":0,"omitted":0}'
    ;;
  malformed-binding)
    printf '%s\n' '{"items":[{"work_id":"work-fixture","focused":true,"control_binding":{"work_id":"work-fixture"}}],"focused_work_id":"work-fixture","total":1,"omitted":0}'
    ;;
  malformed-output)
    printf '%s\n' '{"held":[]}'
    ;;
  *)
    printf '{"items":[{"work_id":"work-newer","short_ref":"w-newer","claim_id":"claim-newer","claim_fence":3,"claimed_at":"2026-09-24T17:00:00Z","expires_at":"2026-09-24T18:00:00Z","focused":false,"control_binding":%s},{"work_id":"work-fixture","short_ref":"w-fixture","claim_id":"claim-fixture","claim_fence":23,"claimed_at":"2026-09-24T16:00:00Z","expires_at":"2026-09-24T18:00:00Z","focused":true,"control_binding":%s}],"focused_work_id":"work-fixture","total":2,"omitted":0}\n' "$newer_binding" "$fixture_binding"
    ;;
esac
exit 0
