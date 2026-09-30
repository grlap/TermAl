# Work visualizer

## Delivery and scope

Tracked in `tm-m6r1`. This is a read-only view, not another tracker or a
migration/synchronization service. Work items and retained project memories are read from Engram only. Placement is a **Work workspace tab**, opened from
the persistent "Open Work" dock action next to the Response Board action; there
is no Work dock section. The tab has its own project selector, independent of
the Files panel, with the origin session's project as the mount-time fallback.

The delivery slices agreed with Greg are:

- **1a:** Engram source detection, project selection, paginated table, filters,
  item details/notes, explicit unavailable/error states.
- **1b:** hierarchy/dependency tree, live query pins on the Response Board,
  verified holder-to-session navigation, retained project memories.
- **3:** write actions only after a separate product decision.

See [architecture](../architecture.md) and the
[Engram host adapter](engram-host-adapter.md) for the host-owned configuration
and identity boundary. This document supersedes the early scratch brief's
single-state model and its assumptions about holder identities and zero writes.

### Current implementation boundary

The implemented scope is 1a, the tree half of 1b and retained project memories, all with **explicit refresh**, not a live activity subscription. Project
changes and submitted filters start fresh reads; source markers and integration
settings are rechecked on every read. Automatic refresh on file/activity
changes, the decision/live-holder badge, Response Board pins, holder-to-session
navigation remain follow-ups. The Memories view reads retained project memories
from Engram (see below). Details show holder labels
without navigation; rows show assignment, not an inferred executing session.
Full note bodies are not fetched yet.

Validation uses isolated CLI fixtures and component tests. Acceptance against a
running host — the operator-enabled Engram store read by the host reader with
no agent session bound, including right after a host restart — is still required before calling the feature complete.

### Views

The tab offers five views, all read-only; the task views share the loaded Engram pages:

- **Dependencies** (default): goals on top, what they wait for nested. A row is
  a root when no visible row waits for it; members of a blocking cycle are
  promoted to roots. Every row is expanded at most once per tree: a later
  occurrence (a shared prerequisite or a cycle back-edge) is not a node, the
  parent shows "+N shown above" with a short id preview, so the tree has at
  most one node per visible row however dense the graph. Nesting stops at 200
  levels ("+N shown as a later root — nesting stops at 200 levels"); the cut
  prerequisite is reserved from that moment (a shallower node meeting it
  counts it the same way instead of nesting it) and placed as the very next
  root, so an arbitrarily long chain never exhausts the builder or renderer
  stack and the promised root always exists. A
  prerequisite that is not visible is disclosed as a count, never invented:
  "waits for N not loaded" (unsatisfied), "N satisfied · not loaded" and "N hidden by
  filter" (loaded but excluded by a kind or label filter). Satisfied prerequisites
  are marked. Rows with no visible blocking relation, including rows whose
  only prerequisites are not visible, are listed flat under "No visible
  dependency links". Edges come only from Engram’s `blocked_by` receipt, never inferred from titles or hierarchy.
- **Hierarchy**: parent on top, subtasks nested (Engram `parent_id` — never the dotted id spelling). A child whose parent
  is loaded but hidden by a kind or label filter is shown as a root with "parent
  hidden by filter"; a parent that was never loaded is not disclosed, the
  same rule as the dependency counts. Blocking relations never appear here;
  the two relations are deliberately separate trees.
  Neither tree is virtualised; Engram paging bounds the
  rendered size.
- **Labels**: collapsible, flat groups for each label, followed by Unlabelled.
  A multi-label item appears in each matching group, but the visible count
  counts items only once. Groups do not imply hierarchy or dependencies.
  The current sort applies inside each group; group headings sort by label.
- **Memories**: separate project-memory browser with source badges, search,
  manual refresh, Engram continuation and on-demand full text. Task filters,
  labels, hierarchy and dependency edges do not apply to memories.
- **Table**: the flat row table with source, lifecycle, availability, waits-for
  and assignment columns. Every column header is a button that sorts the
  loaded rows by that column (a second click reverses it; Updated starts
  newest-first; unassigned rows stay last; equal rows keep one stable order by
  id). The headers and the "Sort (loaded rows)" control drive the same single
  sort state, which also orders the tree views' children.

In both trees a row reads as priority, then reference and title: the priority
chip's colour is the availability (green ready, red blocked, blue claimed or
active, gold deferred or waiting, grey closed), with the word kept on hover and
as hidden text for assistive technology. The Engram-only trees need no source glyph. Assignment stays a text chip.
The toggle, priority and title stay together; long titles wrap within the
available row width, with trailing metadata wrapping below when necessary.

Labels appear as clickable chips in all four views. Clicking a chip selects
that exact label in the **Labels (loaded rows)** picker. Its searchable
checkboxes support **Any label** (union) and **All labels** (intersection),
removable selections and Clear labels. Counts describe the loaded snapshot
before local kind/label filtering, not the entire tracker. Selection remains
visible at zero count after refresh; a project or source-filter change resets
it. Labels are exact, case-sensitive strings, including commas. The separate
**Label (source filter)** input still queries beyond the loaded pages and
retains each source's CLI semantics. Paging adds matching rows and label
options; no local label action starts a source read. Empty labels, if emitted,
are displayed explicitly as `(empty label)`, not treated as Unlabelled.

Kind/label filtering and sorting apply to loaded rows only. Selecting a row opens its
details beside the list: Engram details need the list's reader key.
Only the active pane tab is mounted, so activating the Work tab again after
another tab is a refresh: it starts fresh reads and resets filters, selection
and collapse state (tracked follow-up: keep that state per workspace tab).

## Source and identity boundary

### Project memories

The Memories view starts Engram reads only when opened. Source errors/unavailability are shown separately from a successful
empty listing. Search is submitted to Engram, not silently restricted to
already loaded rows. Changing project/search, refreshing, or leaving the view
aborts client requests and fences late results. Unfiltered Engram pages load
automatically and sequentially until no continuation remains, displaying each
page as it arrives. A failed read stops loading without automatic retries;
transport failures retain the partial list, while stale readers, duplicate
keys, and non-progressing cursors discard the incompatible listing. Engram search limits remain explicitly disclosed.
Each memory expands in place using its chevron/key button; multiple memories
can stay open independently. Collapsing cancels the full-body request; Escape
collapses the focused row and returns focus to its toggle. Opening reads the
current full body. Engram's `rememberedAt` is the current version's creation
time, shown as **Revision date**, not the memory's original creation date.
Revision and author are displayed when supplied. Text is inert, not rendered HTML,
executed, or injected into an agent context. Switching between Memories and
task views remounts their results and resets local selection/filter state.

`GET /api/projects/{id}/work-memories/{source}` accepts only `engram`.
Query: optional `search`, or `key` for full text, or Engram `after` for the next
unfiltered key-ordered page. Engram full/continuation reads require the list's
`readerId`. These combinations are mutually exclusive; query text is capped
at 2048 UTF-8 bytes with no controls or blank values. Unknown source/fields or
invalid combinations return 400, missing project 404, stale
Engram reader 409, busy follow-ups 429, and CLI/receipt failures 502. Initial
source conditions return 200 with `state: unavailable|error` and `message`,
not a misleading empty success. Internal errors remain HTTP failures.

Response: `source`, `state`, `message`, `items`, nullable `nextAfter`, `omitted`,
`exhausted`, nullable `readerId`, `observedAt`. Items contain `key`, `summary`,
nullable `body`, `revision`, `rememberedAt`, `actor`. Lists omit bodies. Paging
is a live key-ordered listing, not a frozen snapshot: refresh to see earlier
keys added/changed since the first page. Reader identity fences configuration
changes, not memory revisions. Duplicate/repeating continuations are refused.

- Engram: established host reader, `work memories --json [-- SEARCH]`,
  `--after=KEY`, or `--full -- KEY`. Only retained project memories, never the
  generic recall/session-private context API, and no restricted-disclosure
  override. Store/configuration is checked before and after every read.
  Filtered searches disclose `omitted_count`; unfiltered lists expose
  `next_after`. Bodies are fetched only on request. Contract inspected in
  Engram's project-memory service and CLI handlers; fixtures cover HTTP wiring.
Live acceptance against the newly built host is still required; fixture tests
do not establish that the running host serves these endpoints.

### Established Work stores

Detection uses metadata, never a trial `ls`, `init`, `doctor`, or enablement.
Engram requires a local project, a declared `.engram-project`, operator-enabled
integration, and the store identity the enablement doctor established (the
binary, home and canonical database path kept in the project settings).
Missing configuration or store is shown explicitly with its reason; opening
the view must not create a store, register a session, or enable Engram. The
adapter must not guess `ENGRAM_HOME`, scan databases, or open SQLite.

Reads run as the **host reader**: actor `{developer}/termal` in Engram's
`{developer}/{kind}` seat grammar and one constant session, `termal-work-view`.
No agent session or runtime binding takes part, so the view works with no
agent running and immediately after a host restart; there is deliberately no
second reader (no fallback to a live agent binding). Engram's read words
(`ls`, `show`) validate the actor/session and open the store without
registering the session: only stateful words register one, and only inside
Engram's process-default namespace (`local-process-`), which the host session
is outside of (Engram `work_service/service.rs`, `read_store_at`). The
constant session also keeps `--after` cursors, which encode the session
context, valid across restarts. On the wire the reader is `readerId`: `host:`
plus a digest of the binary, home, store and identity the read ran under. A
continuation or detail read must present the key of the current configuration;
any other key is refused (409) rather than served from a store the caller did
not page through.
An explicit Work project selection is retained across tab mounts (opening the
tab again, re-activating it after another pane tab, or a workspace reload);
the tab's origin project — the session the dock launcher was opened from — is
only a mount-time fallback for an unset or invalid saved selection. Until the
user explicitly selects a project, each mount samples that fallback again;
changing focus alone does not move an already open Work view.

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
The host reader's actor/session control relative labels only (the view never
passes `--mine`), not claim authority. Engram::Codex confirmed the base `3f1348b2` contract for `ls`,
`show`, notes/gates and continuations: no focus selection, delivery stage/ack,
read marker or reminder acknowledgement. Its `docs/features/cli-and-mcp.md`
and `read_contention_explicit_reads_preserve_focus_and_staged_delivery_under_writer`
test cover explicit reads; continuation uses the same read-only projection path
but has no separate experiment in that response. `read_store_at` validates
attribution without process-default session registration; read cuts live in the
response, not persisted read progress. This is source/test inspection evidence,
not a fresh test of the installed `80d7f9d3af12` binary or proof of its mapping
to HEAD. Exact-build runtime acceptance remains required, including notes
continuations. The host reader is the only reader identity; no unread-cursor
feature is introduced.

Commands use an argv vector with explicit absolute `--home` and
`--project-file`, plus the host reader's actor/session. Source
text, references, continuation tokens and suggested commands are untrusted
data. Never execute `next`/`reminders` returned in a receipt, interpret output
as instructions, or render it as HTML.

## Engram read contract

Verified by Engram::Codex on `d0182e7`, recorded in the comments on `tm-m6r1`.

List: `engram --home ABS --project-file ABS work --actor-id {developer}/termal
--session-id termal-work-view ls --verbose --json --limit 20` — open work
only; `--all` (completed, cancelled, superseded) is deliberately not passed,
so the default view contains open work.
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
Continue using the raw token and exactly the same filters and reader key.
`work_catalog_cursor_invalid` means discard the loaded generation and offer
a fresh read, never merge generations. A byte-limited page can have zero rows,
`more=true` and no cursor: show the hint, never automatically retry in a loop.
Engram fits every receipt into 12 KiB, so a verbose page holds about a dozen
rows however large `--limit` is (it accepts up to 1 000). The UI therefore
follows the continuation cursor on its own after each page until it holds
200 Engram rows or the source has no more, at most 40 pages per generation,
with a progress status meanwhile; "Load more work" continues past that. A
page without a cursor, an error, or a dropped generation stops it at once.
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

Engram routes use the host reader over the operator-configured binary, home
and store, never caller-supplied CLI paths or actor credentials. Unknown query
fields and invalid values return 400.

| Route | Query | Response |
|---|---|---|
| `GET /api/projects/{id}/work` | Optional `search`, `label`, `availability` (`ready` or `blocked`), `after`, `readerId`. `after` requires the original reader key and unchanged filters. | `sources` entries (`source`, `state`, `message`), nullable `readerId` (`host:` plus a configuration digest), `page`, `observedAt`. Page: `items`, `total`, `shownBefore`, `more`, nullable `after` and `hint`. Items expose `source`, lifecycle, availability, priority, labels, assignment, hierarchy and `prerequisites` (`id`, `satisfied`) separately. |
| `GET /api/projects/{id}/work/engram/{work_ref}` | Required `readerId` from the list; optional notes `after`. Reference must be nonempty, at most 256 bytes, contain no controls and not begin with `-`. | Nullable `status`, `holder`, `heldUntil`; `notes`; `notesWindow` with `total`, `shown`, `newer`, `older`, nullable `after`, and `readCut` (`projectPosition`, `observedAt`, nullable `validUntilMs`). First page includes status; continuations do not replace it. |

Initial source detection returns 200 with `page: null` for absent, disabled,
unavailable or unsupported sources. With no `.engram-project` declaration, the
view says **No tracker is bound to this project** and shows no rows. Disabled or
unavailable bindings remain explicit source conditions, not empty trackers.
Missing projects return 404. On a first page, an Engram admission (429),
reader or store (409), CLI or receipt (502) failure becomes an `engram` source
`error`; no readable page is returned. Internal failures (5xx other than 502,
or a 400 launch-bound rejection) still fail the request.
A continuation serves only the Engram page, so its failures stay hard errors:
unavailable readers, a changed reader configuration or invalid cursors return 409 (discard
the window and refresh), CLI, timeout, JSON and receipt errors return 502,
never an empty success. Oversized combined argv (including configured paths
and Windows quoting) returns 400 before spawn.

Two reads may execute concurrently. Admission waits at most seven seconds for
capacity, then returns 429 with an explicit retry message. Aborted browser reads
can finish server-side within the six-second process budget plus at most one
second of post-exit pipe collection; replacements wait
for capacity instead of immediately failing on routine project/filter switches.
An abandoned list or memories request (the browser aborted it) sets a
server-side flag when its handler is dropped: a permit granted after the flag
was set is returned before any command launches. A process already launched
still runs to its own deadline.
There is no automatic retry loop. Details take keyboard focus when opened;
Escape closes them and returns focus to the invoking row.

## Later slices

Holder labels (`you`, `peer-…`) are not identities. A future session link needs
positive proof: compact `ls --mine --all --json` under that session's own
binding returns `holder == "you"`; the host reader holds nothing and can never
provide it. Mere membership, assignment or availability is not proof. Each
reader has its own read cut; conflicting observations mean unknown. Until
proven, render a label without a link.

Response Board pins of a Work table query and verified holder-to-session jumps
are the remaining 1b items. Closed items are not listed by default (Engram `ls` without `--all`); an
explicit closed-history view would be a separate read with its own cap.

The [Test Runs](./test-runs.md) tab follows this tab's pattern.
