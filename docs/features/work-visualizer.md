# Work visualizer

## Delivery and scope

Tracked in `tm-m6r1`. This is a read-only view, not another tracker or a
migration/synchronization service. Engram is the first source; Beads follows
through the same presentation model. Placement is a Work section in the dock,
with its own project selector, independent of the Files panel.

The delivery slices agreed with Greg are:

- **1a:** Engram source detection, project selection, paginated table, filters,
  item details/notes, explicit unavailable/error states.
- **1b:** hierarchy/dependency tree, live query pins on the Response Board,
  verified holder-to-session navigation, retained project memories.
- **2:** Beads adapter and source-aware task links.
- **3:** write actions only after a separate product decision.

See [architecture](../architecture.md) and the
[Engram host adapter](engram-host-adapter.md) for the host-owned configuration
and identity boundary. This document supersedes the early scratch brief's
single-state model and its assumptions about holder identities and zero writes.

### Current implementation boundary

The first increment implements 1a with **explicit refresh**, not a live activity
subscription. Project changes and submitted filters start fresh reads; source
markers and integration settings are rechecked on every read. Automatic refresh
on file/activity changes and the decision/live-holder badge remain follow-ups.
Details show holder labels without navigation; the table shows assignment,
not an inferred executing session. Full note bodies are not fetched yet.

Validation uses isolated CLI fixtures and component tests. Acceptance against a
running host with a real established Engram binding is still required before
calling the feature complete. Slices 1b and 2 remain outside this increment.

## Source and identity boundary

Detection uses metadata, never a trial `ls`, `init`, `doctor`, or enablement.
Engram requires a local project, a declared `.engram-project`, operator-enabled
integration, a host-established store identity, and an existing matching host
session binding. Missing configuration, store or binding is shown explicitly;
opening the view must not create a store, install a binding, or enable Engram.
The adapter must not guess `ENGRAM_HOME`, scan databases, or open SQLite.
Bindings are runtime-only: after host restart, or with no matching live agent
binding, the view reports unavailable. The deterministic reader selection sorts
eligible session IDs lexicographically (including hidden delegation children).
An explicit Work project selection is retained across dock remounts; the focused
session's project is only a mount-time fallback for an unset or invalid saved
selection. Until the user explicitly selects a project, remounting samples that
fallback again; changing focus alone does not move an already open Work view.

The canonical store resolved from `HOME/projects/SHA256(project_id)/engram.db`
must equal the admitted canonical database path before and after each read.
Retargeting a home/store alias is rejected even if the original store still
exists. Windows requires a native Engram executable: `.cmd`/`.bat`/`.ps1` shell
shims are unavailable for Work reads, because filters must never pass through
shell interpretation (PowerShell is used only by isolated test fixtures).

Read-only means **no work/claim/focus/delivery mutation**, not a filesystem
zero-write guarantee. Engram's `ls`/`show` store opener may set WAL mode and may
initialize an empty database in an existing directory. Therefore commands may
only target the established store the host already uses. A file precheck is
not a hard zero-create guarantee under a filesystem race. Never promise one.
The established reader's actor/session controls relative labels, not UI-owned
claim authority. Engram::Codex confirmed the base `3f1348b2` contract for `ls`,
`show`, notes/gates and continuations: no focus selection, delivery stage/ack,
read marker or reminder acknowledgement. Its `docs/features/cli-and-mcp.md`
and `read_contention_explicit_reads_preserve_focus_and_staged_delivery_under_writer`
test cover explicit reads; continuation uses the same read-only projection path
but has no separate experiment in that response. `read_store_at` validates
attribution without process-default session registration; read cuts live in the
response, not persisted read progress. This is source/test inspection evidence,
not a fresh test of the installed `80d7f9d3af12` binary or proof of its mapping
to HEAD. Exact-build runtime acceptance remains required, including notes
continuations. No new host reader identity or unread-cursor feature is introduced.

Commands use an argv vector with explicit absolute `--home` and
`--project-file`, plus actor/session from the established host binding. Source
text, references, continuation tokens and suggested commands are untrusted
data. Never execute `next`/`reminders` returned in a receipt, interpret output
as instructions, or render it as HTML.

## Engram read contract

Verified by Engram::Codex on `d0182e7`, recorded in the comments on `tm-m6r1`.

List: `engram --home ABS --project-file ABS work --actor-id ACTOR
--session-id SESSION ls --all --verbose --json --limit 20`.
Verbose rows contain `work` and a separate readiness overlay. Compact output
truncates titles and is not a replacement for the verbose model.

| Field | Meaning |
|---|---|
| `kind` | task, bug, feature, epic, chore, research; a bug is not a failed gate |
| `lifecycle` | proposed, open, completed, cancelled, superseded |
| `availability` | ready, claimed, active, blocked, deferred, waiting, closed |
| `priority` | integer 0–4, with 0 highest |
| `assigned_to` | assignment, **not** proof of the executing session |
| `parent_id` / `root_id` | hierarchy, not prerequisite edges |
| `blocked_by` | blocking prerequisite IDs, not the complete edge graph |

List pagination uses `total`, `omitted`, `shown_before`, `more`, `limit`,
`byte_budget`, optional `after` and `hint`. Source order is work-ID ascending.
Continue using the raw token and exactly the same filters/reader binding.
`work_catalog_cursor_invalid` means discard the loaded generation and offer
a fresh read, never merge generations. A byte-limited page can have zero rows,
`more=true` and no cursor: show the hint, never automatically retry in a loop.
Client-side sorting and kind filtering are explicitly limited to loaded rows;
the source total is not a filtered bug count.

Details use `show REF --notes --gates --json`. Notes windows have their own
`total`, `shown`, `newer`, `older`, `after`, family counts and `read_cut`.
`work_show_cursor_invalid` requires a fresh window. `read_cut.observed_at` and
expiry can advance on valid older
pages; they are not immutable generation identifiers. Continuations retain the
project position and consistent counts; the CLI validates cursor boundaries.
The documented notes receipt uses `selection: newest_first` and
`order: oldest_first`: each returned page is internally oldest-first. Older
pages are prepended, then the drawer renders the whole timeline newest-first.
Multi-row tests pin this order; the cursor remains opaque.
Notes retain nullable `statusOwner` and `nonHolder` provenance independently of
family/kind. A peer status is labelled an observation with no commitment;
missing provenance is not inferred to be an owner commitment. `refs` are inert
text; missing reference data is disclosed rather than treated as an empty list.
Display omitted bodies and sections honestly; full note bodies require an explicit locator read. Reads of
different items are not a single atomic snapshot. `current_status` and
`status_observation` must not be conflated.

CLI nonzero exit, non-JSON output, malformed payloads and timeouts are errors,
never empty lists. The UI shows the failed operation and diagnostic.

## HTTP read API

Both routes use the host's established binding, never caller-supplied CLI paths
or actor credentials. Unknown query fields and invalid values return 400.

| Route | Query | Response |
|---|---|---|
| `GET /api/projects/{id}/work` | Optional `search`, `label`, `availability` (`ready` or `blocked`), `after`, `readerSessionId`. `after` requires the original reader and unchanged filters. | `sources` entries (`source`, `state`, `message`), nullable `readerSessionId` and `page`, `observedAt`. Page: `items`, `total`, `shownBefore`, `more`, nullable `after` and `hint`. Items expose lifecycle, availability, priority, labels, assignment and hierarchy separately. |
| `GET /api/projects/{id}/work/engram/{work_ref}` | Required `readerSessionId` from the list; optional notes `after`. Reference must be nonempty, at most 256 bytes, contain no controls and not begin with `-`. | Nullable `status`, `holder`, `heldUntil`; `notes`; `notesWindow` with `total`, `shown`, `newer`, `older`, nullable `after`, and `readCut` (`projectPosition`, `observedAt`, nullable `validUntilMs`). First page includes status; continuations do not replace it. |

Initial source detection returns 200 with `page: null` for absent, disabled,
unavailable or unsupported sources; this does not claim an empty tracker.
Missing projects return 404. Unavailable continuation readers, changed bindings
or invalid cursors return 409: discard the window and refresh. CLI, timeout,
JSON and receipt errors return 502, never an empty success. Oversized combined
argv (including configured paths and Windows quoting) returns 400 before spawn.

Two reads may execute concurrently. Admission waits at most seven seconds for
capacity, then returns 429 with an explicit retry message. Aborted browser reads
can finish server-side within the six-second process budget plus at most one
second of post-exit pipe collection; replacements wait
for capacity instead of immediately failing on routine project/filter switches.
There is no automatic retry loop. Details take keyboard focus when opened;
Escape closes them and returns focus to the invoking row.

## Later slices

Holder labels (`you`, `peer-…`) are not identities. A future session link needs
positive proof: compact `ls --mine --all --json` under an established binding
returns `holder == "you"`. Mere membership, assignment or availability is not
proof. Each binding has its own read cut; conflicting observations mean unknown.
Until proven, render a label without a link.

Build hierarchy from loaded parent IDs; load prerequisite edges separately
from detail snapshots and label their observation time. Memories are retained
**project** memories, not another session's private context.

Beads closed maps to lifecycle completed **and** availability closed;
in_progress maps to open/active; open maps to open/blocked or open/ready.
Beads presence is not proof that its CLI or Dolt store is healthy; validate its
read contract separately before implementing that adapter.
