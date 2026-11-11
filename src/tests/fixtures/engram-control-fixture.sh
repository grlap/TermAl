#!/bin/sh
set -eu

project_file=""
engram_home=""
actor_id=""
actor_context=""
session_id=""
is_doctor=0
is_readiness=0
is_peek=0
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
  if [ "$1" = "--actor-id" ]; then
    actor_id="$2"
    shift 2
    continue
  fi
  if [ "$1" = "--actor-context" ]; then
    actor_context="$2"
    shift 2
    continue
  fi
  if [ "$1" = "--session-id" ]; then
    session_id="$2"
    shift 2
    continue
  fi
  if [ "$1" = "doctor" ]; then
    is_doctor=1
  fi
  if [ "$1" = "readiness" ]; then
    is_readiness=1
  fi
  if [ "$1" = "--peek" ]; then
    is_peek=1
  fi
  shift
done
[ -n "$project_file" ] || exit 2
[ -n "$engram_home" ] || exit 2
# This fixture writes a placeholder store and marker files under --home. Every
# TermAl test temp root lives below <user temp>/termal/tests, with the user
# temp derived as test_temp_dir() derives it: TERMAL_TEST_USER_TEMP, else
# std::env::temp_dir(), which is TMPDIR when set, else the confstr
# DARWIN_USER_TEMP_DIR directory on macOS, else /tmp. Any other home is a real
# Engram home (a developer's ~/.engram received one once). Refuse before any
# write; exit 4 is used by no other exit in either fixture script. The root is
# resolved with physical cd (including macOS's /var alias), matching the
# canonical TestTempRoot. Keep the logical spelling too for public paths
# passed by the connection resolver. Each spelling is compared at a component
# boundary and any `..` segment is refused outright, since nothing normalises
# the home here.
refuse_engram_home() {
  echo "engram-control-fixture: refusing --home outside a TermAl test temp root ($test_root): $engram_home" >&2
  exit 4
}
user_temp=${TERMAL_TEST_USER_TEMP:-${TMPDIR:-}}
if [ -z "$user_temp" ]; then
  case "$(uname -s 2>/dev/null || true)" in
    Darwin) user_temp=$(getconf DARWIN_USER_TEMP_DIR 2>/dev/null || true) ;;
  esac
  [ -n "$user_temp" ] || user_temp=/tmp
fi
test_root=$(cd "$user_temp" 2>/dev/null && pwd -P) || test_root=""
[ -n "$test_root" ] || refuse_engram_home
test_root="${test_root%/}/termal/tests"
logical_test_root=$(cd "$user_temp" 2>/dev/null && pwd -L) || refuse_engram_home
logical_test_root="${logical_test_root%/}/termal/tests"
case "/$engram_home/" in
  */../*) refuse_engram_home ;;
esac
case "$engram_home" in
  "$test_root"|"$test_root"/*|"$logical_test_root"|"$logical_test_root"/*) ;;
  *) refuse_engram_home ;;
esac

case " $original_args " in
  *" work "*" next "*)
    [ -n "${ENGRAM_HOME:-}" ] || exit 8
    [ -n "${ENGRAM_ACTOR_ID:-}" ] || exit 8
    [ -n "${ENGRAM_ACTOR_CONTEXT:-}" ] || exit 8
    [ -n "${ENGRAM_SESSION_ID:-}" ] || exit 8
    [ "$ENGRAM_HOME" = "$engram_home" ] || exit 9
    [ "$ENGRAM_ACTOR_ID" = "$actor_id" ] || exit 9
    [ "$ENGRAM_ACTOR_CONTEXT" = "$actor_context" ] || exit 9
    [ "$ENGRAM_SESSION_ID" = "$session_id" ] || exit 9
    mode=$(tr -d '\r\n' < "$project_file")
    printf "next\n" >> "$engram_home/work-context-reads"
    printf '%s\n' "$original_args" > "$engram_home/work-context-args.txt"
    if [ "$mode" = "fixture-work-next-delivery" ]; then
      # Model the documented peek/next boundary, independently of host cache.
      page=0
      if [ -f "$engram_home/work-delivery-cursor" ]; then
        page=$(cat "$engram_home/work-delivery-cursor")
      fi
      if [ "$is_peek" -eq 0 ]; then
        printf '%s' "$((page + 1))" > "$engram_home/work-delivery-cursor"
      fi
      printf 'delivery-page-%s\n' "$page"
      awk 'BEGIN { for (i = 0; i < 40000; i++) printf "x"; print "" }'
      printf 'complete-page-tail\n'
      exit 0
    fi
    if [ "$mode" = "fixture-work-next-gated" ] && [ ! -e "$engram_home/work-context-released" ]; then
      sh "$engram_home/work-context-gate.sh" >/dev/null
    fi
    printf 'Engram work context for %s as %s\n' "$ENGRAM_SESSION_ID" "$ENGRAM_ACTOR_ID"
    exit 0
    ;;
esac

if [ "$is_doctor" -eq 1 ] || [ "$is_readiness" -eq 1 ]; then
  if [ "$is_readiness" -eq 1 ]; then diagnostic=readiness; else diagnostic=doctor; fi
  printf '%s\n' "$diagnostic" >> "$engram_home/diagnostic-commands"
  mode=$(sed 's/^[[:space:]]*//; s/[[:space:]]*$//' "$project_file" | tr -d '\r\n')
  if [ "$mode" = 'fixture-diagnostic-large-refusal' ]; then
    awk 'BEGIN { printf "invalid-json-"; for (i = 0; i < 100000; i++) printf "x"; print "" }'
    awk 'BEGIN { printf "warning-"; for (i = 0; i < 100000; i++) printf "y"; print "" }' >&2
    exit 1
  fi
  if [ "$mode" = 'fixture-diagnostic-marker-oversized' ]; then
    awk 'BEGIN { for (i = 0; i < 4097; i++) printf "x" }' > "$project_file"
  fi
  if [ "$is_readiness" -eq 1 ]; then
    case "$mode" in
      fixture-readiness-old) printf 'unknown subcommand readiness\n' >&2; exit 2 ;;
      fixture-readiness-malformed) printf '{}\n'; exit 0 ;;
      fixture-report-temp) printf 'TEMP=%s\nTMP=%s\n' "${TEMP:-}" "${TMP:-}"; exit 0 ;;
      fixture-readiness-refusal) printf '{"ready":true}\n'; exit 1 ;;
    esac
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    hash=$(printf '%s' "$mode" | sha256sum | cut -d ' ' -f 1)
  else
    hash=$(printf '%s' "$mode" | shasum -a 256 | cut -d ' ' -f 1)
  fi
  database="$engram_home/projects/$hash/engram.db"
  mkdir -p "$(dirname "$database")"
  [ -f "$database" ] || printf 'fixture database' > "$database"
  if [ "$mode" = 'fixture-diagnostic-large-report' ]; then
    awk 'BEGIN { printf "warning-"; for (i = 0; i < 100000; i++) printf "y"; print "" }' >&2
    printf '{"healthy":true,"database":"%s","project_id":"%s","detail":"' "$database" "$mode"
    awk 'BEGIN { for (i = 0; i < 100000; i++) printf "x" }'
    printf '"}\n'
    exit 0
  fi
  if [ "$is_readiness" -eq 1 ]; then
    printf '{"schema_version":1,"scope":"readiness","ready":true,"full_audit":"not_run","mutation_enabled":false,"work_schema_version":1,"host_path_policy":{"stored":"fixture","resolved":"fixture","status":"matched"},"database":"%s","project_id":"%s"}\n' "$database" "$mode"
    exit 0
  fi
  if [ "$mode" = 'fixture-audit-unhealthy' ]; then
    printf '{"healthy":false,"database":"%s","project_id":"%s"}\n' "$database" "$mode"
    printf 'development redactor provides no protection\n' >&2
    exit 1
  fi
  printf '{"healthy":true,"database":"%s","project_id":"%s"}\n' "$database" "$mode"
  exit 0
fi

[ -n "$actor_id" ] || exit 10
[ -n "$session_id" ] || exit 10
[ "${ENGRAM_HOME:-}" = "$engram_home" ] || exit 10
[ "${ENGRAM_ACTOR_ID:-}" = "$actor_id" ] || exit 10
[ "${ENGRAM_ACTOR_CONTEXT:-}" = "$actor_context" ] || exit 10
[ "${ENGRAM_SESSION_ID:-}" = "$session_id" ] || exit 10

json_field() {
  printf '%s' "$1" | sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p"
}

write_error() {
  printf '{"status":"error","error":{"code":"%s","message":"%s"}}\n' "$1" "$2"
}

# Readiness is a pipe publication, not a delay for the child to start.
initial_mode=$(tr -d '\r\n' < "$project_file")
case "$initial_mode" in
  fixture-tree-*)
    ready_pipe="$engram_home/descendant-ready.pipe"
    mkfifo "$ready_pipe"
    sh "$engram_home/engram-descendant.sh" > "$ready_pipe" &
    exec 3< "$ready_pipe"
    while IFS= read -r ready_line <&3; do
      case "$ready_line" in *termal-descendant-ready*) break ;; esac
    done
    case "$ready_line" in *termal-descendant-ready*) ;; *) exit 3 ;; esac
    ;;
esac
case "$initial_mode" in
  fixture-stderr-*)
    # Exceed pipe capacity before readiness; arbitrary text stays private.
    printf '%s' "thread 'fixture' panicked at PRIVATE_PATH token=PRIVATE_TOKEN PRIVATE_REQUEST " >&2
    block=$(printf '%1024s' '' | tr ' ' x)
    i=0
    while [ "$i" -lt 128 ]; do printf '%s' "$block" >&2; i=$((i + 1)); done
    if [ "$initial_mode" = fixture-stderr-startup-eof ]; then
      exec 1>&-
      IFS= read -r unused
      exit 0
    fi
    ;;
esac
printf '%s\n' 'termal-engram-control-fixture-ready'

while IFS= read -r line; do
  if [ -f "$engram_home/control-frames.jsonl" ]; then
    printf '%s\n' "$line" >> "$engram_home/control-frames.jsonl"
  fi
  if [ "$(json_field "$line" operation)" = named_root_read ] && [ -f "$engram_home/root-lifecycle-read.json" ]; then
    printf '{"status":"ok","result":%s}\n' "$(cat "$engram_home/root-lifecycle-read.json")"
    continue
  fi
  mode=$(tr -d '\r\n' < "$project_file")
  case "$mode" in
    fixture-stderr-live-eof)
      exec 1>&-
      IFS= read -r unused
      exit 0
      ;;
    fixture-eof)
      exit 0
      ;;
    fixture-hang|fixture-stderr-hang)
      # No response and no timer: the host must kill this blocked reader.
      IFS= read -r unused
      exit 0
      ;;
    fixture-tree-hang)
      IFS= read -r unused
      exit 0
      ;;
    fixture-tree-eof)
      exit 0
      ;;
    fixture-tree-reply)
      if [ "$(json_field "$line" operation)" = work_session_read ]; then
        # Framing-only transport response, not an identity or capability proof.
        printf '%s\n' '{"status":"ok","result":{"binding":null}}'
      else
        write_error invalid_request 'unsupported fixture operation'
      fi
      ;;
    fixture-malformed)
      printf '%s\n' '{"status":'
      ;;
    # Terminal control-startup replies: one flushed error line answering the
    # frame, then a nonzero exit, with no dispatch.
    fixture-startup-busy)
      write_error control_startup_busy 'Engram control startup could not acquire the store.'
      exit 3
      ;;
    fixture-startup-refused)
      write_error control_startup_refused 'Engram control startup was refused.'
      exit 3
      ;;
    # Request-level error replies on a healthy connection, which continues.
    fixture-remote-storage-error)
      write_error storage_error 'COMMIT or rollback failed'
      ;;
    fixture-remote-unknown-code)
      write_error fixture_future_code 'a code this host does not know'
      ;;
    *)
      if [ "$(json_field "$line" operation)" = work_session_read ]; then
        # Framing-only transport response, not an identity or capability proof.
        printf '%s\n' '{"status":"ok","result":{"binding":null}}'
      else
        write_error invalid_request 'unsupported fixture operation'
      fi
      ;;
  esac
done
