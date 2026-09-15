#!/bin/sh
# Beads (bd) read fixture: records argv (last command and a full log), refuses
# anything that is not a --readonly --json read, and answers list / show /
# comments with bd 1.2.2 receipt shapes (every list row carries its edges
# inline; `show` prints an array even for one id).
printf '%s\n' "$@" > beads-read-args.txt
printf '%s\n' "$*" >> beads-read-args-log.txt
printf '%s|%s\n' "${BEADS_DIR:-}" "${BEADS_DB:-}" > beads-read-env.txt
if [ "$1" != "--readonly" ] || [ "$2" != "--json" ]; then echo 'fixture requires --readonly --json first' >&2; exit 9; fi
shift 2
has() { needle="$1"; shift; for a in "$@"; do [ "$a" = "$needle" ] && return 0; done; return 1; }
root_row='{"id":"tm-root","title":"Root epic <script> inert","description":"Epic","status":"in_progress","priority":2,"issue_type":"feature","assignee":"Termal::Codex","owner":"Greg","created_at":"2026-09-01T00:00:00Z","created_by":"Greg","updated_at":"2026-09-13T00:00:00Z","started_at":null,"dependency_count":0,"dependent_count":1,"comment_count":2}'
case "$1" in
  list)
    if [ -e beads-fixture-fail ]; then echo 'Error: database is locked' >&2; exit 1; fi
    if [ -e beads-fixture-malformed ]; then echo 'not json'; exit 0; fi
    if has '--label=decision' "$@"; then printf '[%s]\n' "$root_row"; exit 0; fi
    for a in "$@"; do case "$a" in --label=*) echo '[]'; exit 0;; esac; done
    # tm-root.1 declares two blockers (dependency_count counts `blocks` only,
    # as bd 1.2.2 does) and carries its records inline: two blocks and the
    # parent-child edge. Markers: no-edges drops the records and the parent;
    # partial-edges keeps only the parent-child record; two-dependents makes
    # tm-free declare and carry one blocker of its own.
    child_deps=',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-free","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"}],"parent":"tm-root"'
    if [ -e beads-fixture-no-edges ]; then child_deps=''; fi
    if [ -e beads-fixture-partial-edges ]; then child_deps=',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"}],"parent":"tm-root"'; fi
    # external-blocker: a third blocker whose reference is not a Beads
    # identifier (a cross-project reference), declared and carried.
    child_count=2
    if [ -e beads-fixture-external-blocker ]; then child_count=3; child_deps=',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-free","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"external:other:tm-1","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"}],"parent":"tm-root"'; fi
    # malformed-record: the full records plus one that does not parse (no type).
    if [ -e beads-fixture-malformed-record ]; then child_deps=',"dependencies":[{"issue_id":"tm-root.1","depends_on_id":"tm-free","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-root","type":"parent-child","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","metadata":"{}"},{"issue_id":"tm-root.1","depends_on_id":"tm-odd"}],"parent":"tm-root"'; fi
    free_count=0; free_deps=''
    if [ -e beads-fixture-two-dependents ]; then free_count=1; free_deps=',"dependencies":[{"issue_id":"tm-free","depends_on_id":"tm-closed","type":"blocks","created_at":"2026-09-03T00:00:00Z","created_by":"Greg","metadata":"{}"}]'; fi
    printf '[%s,%s]\n' "$root_row" '{"id":"tm-root.1","title":"Blocked child","description":"Waits","status":"open","priority":1,"issue_type":"task","assignee":null,"owner":"Greg","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","updated_at":"2026-09-12T00:00:00Z","started_at":null,"dependency_count":'"$child_count"',"dependent_count":0,"comment_count":0'"$child_deps"'},{"id":"tm-free","title":"Ready bug","description":"","status":"open","priority":3,"issue_type":"bug","assignee":null,"owner":"Greg","created_at":"2026-09-03T00:00:00Z","created_by":"Greg","updated_at":"2026-09-11T00:00:00Z","started_at":null,"dependency_count":'"$free_count"',"dependent_count":1,"comment_count":0'"$free_deps"'}'
    ;;
  show)
    if [ -e beads-fixture-malformed-show ]; then echo 'not json'; exit 0; fi
    if [ -e beads-fixture-fail-show ]; then echo 'Error: database is locked' >&2; exit 1; fi
    # An unknown-id line beside a store failure: not a refusal of unknown ids.
    if [ -e beads-fixture-mixed-refusal-show ]; then echo 'Error fetching tm-missing: no issue found matching "tm-missing"' >&2; echo 'Error: database is locked' >&2; exit 1; fi
    if [ -e beads-fixture-same-line-refusal-show ]; then echo 'Error fetching tm-missing: no issue found matching "tm-missing" (database is locked)' >&2; exit 1; fi
    if [ -e beads-fixture-aggregate-refusal-show ]; then echo 'Error fetching tm-missing: no issue found matching "tm-missing"' >&2; printf '{\n  "error": "no issues found matching the provided IDs",\n  "schema_version": 1\n}\n'; exit 1; fi
    # The unknown-id refusal as a JSON error on stdout beside an unrelated
    # stderr notice: the classification must read both streams.
    if [ -e beads-fixture-unknown-show-json ]; then echo 'warning: a newer bd is available' >&2; echo '{"error":"Error fetching tm-missing: no issue found matching \"tm-missing\"","schema_version":1}'; exit 1; fi
    # bd 1.2.2 semantics: unknown ids are omitted from the receipt (with a
    # stderr line each); a receipt with no known id exits 1; the receipt is an
    # array even for one id. `tm-root` resolves to the tm-root.1 object, like
    # a prefix/alias lookup returning another issue.
    closed='{"id":"tm-closed","title":"Closed blocker","description":"","status":"closed","priority":2,"issue_type":"task","assignee":null,"owner":"Greg","created_at":"2026-08-01T00:00:00Z","created_by":"Greg","updated_at":"2026-08-02T00:00:00Z","started_at":null,"dependent_count":1,"dependency_count":0,"comment_count":0}'
    child='{"id":"tm-root.1","title":"Blocked child","description":"Waits for <b>inert</b>","status":"open","priority":1,"issue_type":"task","assignee":null,"owner":"Greg","created_at":"2026-09-02T00:00:00Z","created_by":"Greg","updated_at":"2026-09-12T00:00:00Z","started_at":null,"dependencies":[{"id":"tm-root","title":"Root epic <script> inert","status":"in_progress","priority":2,"issue_type":"feature","dependency_type":"parent-child"},{"id":"tm-free","title":"Ready bug","status":"open","priority":3,"issue_type":"bug","dependency_type":"blocks"}],"parent":"tm-root","dependent_count":0,"dependency_count":2,"comment_count":1}'
    shift
    found=''; n=0
    for id in "$@"; do
      case "$id" in
        tm-closed) obj="$closed";;
        tm-root.1|tm-root) obj="$child";;
        *) echo "Error fetching $id: no issue found matching \"$id\"" >&2; continue;;
      esac
      if [ -z "$found" ]; then found="$obj"; else found="$found,$obj"; fi
      n=$((n + 1))
    done
    if [ "$n" -eq 0 ]; then exit 1; fi
    printf '[%s]\n' "$found"
    ;;
  comments)
    # Marker: a comment recorded for another issue than the one requested.
    issue='tm-root.1'; if [ -e beads-fixture-stray-comment ]; then issue='tm-other'; fi
    printf '[{"id":"c-1","issue_id":"%s","author":"Greg Lapinski","text":"First comment <i>inert</i>","created_at":"2026-09-12T10:00:00Z"}]\n' "$issue"
    ;;
  *)
    echo 'fixture: unsupported operation' >&2; exit 7
    ;;
esac
exit 0
