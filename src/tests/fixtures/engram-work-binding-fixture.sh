#!/bin/sh
set -eu

project_file=""
engram_home=""
actor_id=""
actor_context=""
session_id=""
operation=""
work_ref=""
sections=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --project-file) project_file=$2; shift 2 ;;
    --home) engram_home=$2; shift 2 ;;
    --actor-id) actor_id=$2; shift 2 ;;
    --actor-context) actor_context=$2; shift 2 ;;
    --session-id) session_id=$2; shift 2 ;;
    --sections) sections=$2; shift 2 ;;
    work) shift ;;
    next) operation=next; shift ;;
    focus)
      if [ "$operation" = next ]; then
        sections=focus
        shift
      else
        operation=focus
        work_ref=$2
        shift 2
      fi
      ;;
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
if [ "$operation" = focus ]; then
  printf 'focus:%s\n' "$work_ref" >> "$engram_home/work-read-phases"
else
  printf '%s\n' "$operation" >> "$engram_home/work-read-phases"
fi
marker="$engram_home/work-next-read"
lock_retry_marker="$engram_home/work-lock-retried"
if [ "$operation" = next ]; then
  [ "$sections" = focus ] || exit 4
  if [ "$mode" = read-error ]; then
    printf '%s\n' 'database is locked' >&2
    exit 6
  fi
  if [ "$mode" = read-error-once ] && [ ! -e "$lock_retry_marker" ]; then
    printf retry > "$lock_retry_marker"
    printf '%s\n' 'database is locked' >&2
    exit 6
  fi
  printf ready > "$marker"
  if [ "$mode" = no-focus ]; then
    printf '%s\n' '{"session":{},"focus":null}'
  else
    printf '%s\n' '{"session":{},"focus":{"status":{"work":{"work_id":"work-fixture"}}}}'
  fi
  exit 0
fi
if [ "$operation" = focus ] && [ "$work_ref" = work-fixture ] && [ -e "$marker" ]; then
  printf '%s\n' '{"control_binding":{"root_execution_id":"root-fixture","work_id":"work-fixture","run_id":"run-fixture","work_revision":17,"claim_id":"claim-fixture","claim_fence":23}}'
  exit 0
fi
exit 5
