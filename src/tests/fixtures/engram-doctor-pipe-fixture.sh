#!/bin/sh
set -eu
# Owns doctor descendant/pipe-lifetime fixtures; no real store is opened.
project_file=""
engram_home=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --project-file) project_file="$2"; shift 2 ;;
    --home) engram_home="$2"; shift 2 ;;
    *) shift ;;
  esac
done
ready_pipe="$engram_home/doctor-ready.pipe"
mkfifo "$ready_pipe"
# Only stderr is inherited: stdout is a readiness handshake, not doctor JSON.
sh "$engram_home/engram-descendant.sh" > "$ready_pipe" &
child=$!
while IFS= read -r line; do
  case "$line" in *termal-descendant-ready*) break ;; esac
done < "$ready_pipe"
printf '%s' '{"healthy":true,"database":"/fixture/engram.sqlite","project_id":"doctor-pipe"}'
if [ "$(tr -d '\r\n' < "$project_file")" = "doctor-tree-hang" ]; then
  wait "$child"
fi
