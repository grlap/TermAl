#!/bin/sh
fixture_home=''
previous=''
fixture_show=false
fixture_after=''
for arg in "$@"; do
  if [ "$previous" = '--home' ]; then fixture_home="$arg"; fi
  if [ "$arg" = 'show' ]; then fixture_show=true; fi
  case "$arg" in --after=*) fixture_after="${arg#--after=}" ;; esac
  previous="$arg"
done
printf '%s\n' "$@" > "$fixture_home/work-read-args.txt"
fixture_memories=false
fixture_full=false
for arg in "$@"; do
  [ "$arg" = memories ] && fixture_memories=true
  [ "$arg" = --full ] && fixture_full=true
done
if [ "$fixture_memories" = true ]; then
  if [ "$fixture_full" = true ]; then printf '%s\n' '{"key":"guide","revision":2,"body":"<script>inert</script>","remembered_at":"2026-09-16T00:00:00Z","actor_id":"test/termal"}'; exit 0; fi
  if [ "$fixture_after" = guide ]; then printf '%s\n' '{"memories":[{"key":"later","revision":1,"first_line":"Later memory","remembered_at":"2026-09-16T00:00:00Z","actor_id":"test/termal"}],"omitted_count":0,"exhausted":true}'; exit 0; fi
  printf '%s\n' '{"memories":[{"key":"guide","revision":2,"first_line":"Summary","remembered_at":"2026-09-16T00:00:00Z","actor_id":"test/termal"}],"next_after":"guide","omitted_count":0,"exhausted":false}'; exit 0
fi
for arg in "$@"; do
  case "$arg" in
    --search=stale) printf '%s\n' 'work_catalog_cursor_invalid: fixture changed' >&2; exit 1 ;;
    --search=malformed) printf '%s\n' 'not json'; exit 0 ;;
  esac
done
if [ "$fixture_show" = true ]; then
  if [ "$fixture_after" = stale ]; then printf '%s\n' 'work_show_cursor_invalid: fixture changed' >&2; exit 1; fi
  fixture_status='"status":{"work":{"short_ref":"w-test","title":"Fixture","outcome":"Goal","acceptance":[],"priority":2,"kind":"bug","lifecycle":"open"},"availability":"ready"},'
  if [ "$fixture_after" = older ]; then fixture_status=''; fi
  printf '{%s"notes":[],"notes_window":{"total":0,"shown":0,"newer":0,"older":0,"read_cut":{"project_position":1,"observed_at":"now"}}}\n' "$fixture_status"
  exit 0
fi
printf '%s\n' '{"items":[{"work":{"work_id":"fixture-id","short_ref":"w-test","title":"Fixture","kind":"bug","lifecycle":"open","priority":2,"labels":[],"assigned_to":null,"parent_id":null,"updated_at":"2026-09-13T00:00:00Z"},"availability":"ready","blocked_by":[]}],"total":1,"shown_before":0,"more":false}'
