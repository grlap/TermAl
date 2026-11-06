# Workspace change notifications

The notify callback inserts into one path-component trie before session routing.
It queues only a wake in a channel of capacity one. Repeated paths merge; more
than 64 children at a node collapse to a covering directory marker. The global
budgets are 4096 nodes and 256 KiB of charged node, edge, key and expanded
delivery storage. Byte/node overflow collapses the deepest ancestor of the
inserted path that frees enough space. Selection follows only that path using
cached subtree totals, so an unrelated deeper path is not selected instead.
If the inserted branch cannot free enough space, a shared ancestor can widen
other branches too; finite storage cannot preserve arbitrary roots exactly.
An oversized/deep path keeps its deepest bounded ancestor. An unplaceable path
or a backend rescan/error becomes a global covering marker. Errors are logged
at most once per 30 seconds; empty non-rescan events do not invalidate. These are storage budgets, not
an RSS or allocator measurement. The notify backend's own allocations are
outside this structure and still require live measurement.

The worker takes a generation after 250 ms of quiet or one second after its
oldest pending arrival. New arrivals never extend the oldest deadline. It checks
after every bounded wake/100 ms timeout, including during continuous traffic.
Filesystem reads, state locks and publications occur after releasing the trie
mutex. An action blocked on those operations can still delay delivery; one
second is a service deadline, not a wall-clock latency guarantee.

A `subtree` change names a covering directory, rather than an exhaustive list
of modified files. An empty subtree path covers every workspace. Rename retains
both old and new paths; directory deletion remains a covering invalidation even
after the directory disappears. No directory traversal reconstructs child paths.
Scope routing uses the delivered path, not a cached source-authority grant.

## Consumers

| Consumer | Owner and handling |
| --- | --- |
| Source editor | `SessionPaneView.source-file.ts`, `workspaceFilesChangedEventChangeForPath`: re-read a covered open file, compare hashes, retain unsaved edits and detect a missing endpoint. |
| Editable diff | `panels/DiffPanel.tsx`, same predicate: re-read the endpoint, use existing rebase/conflict handling; directory markers are never treated as proof of a file deletion. |
| Git diff previews | `git-diff-refresh.ts`, `workspaceFilesChangedEventTouchesGitDiffTab`: refresh covered open requests, including old paths for renames. |
| Git status | `panels/GitStatusPanel.tsx` is not a file-event consumer. It reads Git status on its existing focus/visibility and ten-second polling schedule; Git diff previews above consume notifications. |
| File tree | `panels/FileSystemPanel.tsx`, same root predicate: refresh Git decorations and expanded directory entries. |
| Turn attribution | `file_event_attribution.rs`: match overlap with the captured source root/workdir off-lock, then revalidate ownership; preserve subtree markers in summaries instead of inventing child paths. A global marker keeps its coverage when workdir and source root differ. Bounding accumulation across successive generations is separate work. |
| Engram carried/launching fences | `engram_carried_checks.rs`, `note_engram_observed_workspace_file_changes`: a marker inside a root follows that path's worktree routing, own-scratch exemption and Cargo-sibling candidate handling. Independently it reaches checks rooted under the marker, including nested repositories within a parent's scratch. A global marker reaches all checks. |
| SSE recovery | `api_sse.rs`: a lagged file-event receiver gets a global subtree invalidation. `workspace-file-events.ts` keeps that marker through UI buffering, replacing older exact hints it covers. |

Every merged fact retains its first and last callback arrival, including the
process-local watcher-order points. Fence decisions use the last point: a
pre-launch first notification cannot hide a later write to the same path.
Different paths keep distinct intervals. Callback arrival remains distinct from
actual OS write time; late OS delivery can conservatively fence a run.

Each fact also retains one fixed-size first raw observation for the
[watcher provenance diagnostic](features/watcher-credit-diagnostics.md).
Terminal paths retain their own observation and kind independently of child
facts; only a genuine collapse emits the descendant aggregate. These raw facts
describe the first event, while the last arrival controls the conservative fence.

The opt-in Linux memory comparison measures the combined removal of scope
fan-out and bounded retention using a test-owned legacy model in the candidate
binary. Its 4,000 siblings are a favorable collapse case, not an isolated trie
benchmark or proof of the incident's full host allocation. Separate automated
wide/deep burst tests assert storage bounds, coverage and unrelated-root isolation.
The new persisted `subtree` summary kind may prevent an older backup binary
from reading those summaries; account for this when planning a rollback.

Absolute-ancestor ignore matching is a separate existing issue. This change
retains that filter: placing a checkout below `target` still suppresses its
ordinary events. Bounds tests use virtual paths or direct trie insertion and do
not pretend that placement proves live watcher isolation. Windows full/live
validation must include an ordinary checkout path when that behavior is tested.
