# Feature Brief: Territory Visualization

Backlog source: [`docs/bugs.md`](../bugs.md)

The backlog context and implementation plan below were moved out of `docs/bugs.md`.

## No territory visualization

**Severity:** High â€” the single biggest coordination gap in a multi-agent workflow.

A developer paired with an agent has immense leverage: one person can drive multiple agents across
different parts of a codebase simultaneously. But that leverage collapses without coordination
visibility. Today the user has to hold the full territorial picture in their head â€” which agent is
working where, which files are in flight, whether two sessions are about to collide. That mental
bookkeeping scales badly and is the first thing to break under load.

File edits are buried inside individual conversation streams. There is no cross-session view that
answers "which agent last touched this file?", "what is each agent working on right now?", or "are
two sessions about to collide on the same module?" The developer is the sole coordination layer, and
the tool gives them nothing to coordinate with.

**Why this matters more than most features:**
- The value of TermAl scales with how many agents the developer can run concurrently
- Concurrent agents are only useful if the developer can steer them without constant context-switching
- Territory visualization is the difference between "I'm running three agents" and "I'm effectively
  managing three agents" â€” without it, parallelism becomes chaos
- Conflict detection is not just about preventing git merge pain; it is about preserving the
  developer's trust that concurrent agents are safe to run

**Desired behavior:**
- A dedicated territory view (tab or overlay) shows the project tree annotated with agent activity
- Each file or directory shows which agent(s) have read or written it during the current work session
- Color-coded ownership makes it obvious at a glance: e.g. blue = Claude, orange = Codex,
  green = Gemini, striped = contested (multiple agents touched it)
- Recency matters: recent changes are brighter/bolder, stale activity fades
- Clicking a file in the territory view jumps to the most recent conversation message where that
  file was changed
- A heatmap mode can highlight hotspots â€” files with the most churn across agents
- Conflict warnings surface when two active sessions are both editing the same file or overlapping
  lines
- A compact summary bar (always visible, not just in the territory tab) shows the live territory
  status: e.g. "Claude: 4 files Â· Codex: 7 files Â· 1 conflict"

**Data sources:**
- Diff messages already carry `filePath` and `changeType` per agent turn
- Tool-use events (file reads, writes, command executions) can be tagged with session and agent
- Git status can supplement the view with uncommitted changes not yet attributed to an agent

**Tasks:**
- Track file-level read/write activity per session in backend state (agent, session, file, action,
  timestamp)
- Add a `/api/territory` endpoint that returns the aggregated activity map
- Add a territory view tab using the generic workspace tab system
- Render a project tree with agent-colored annotations and recency decay
- Add a heatmap toggle that ranks files by cross-agent churn
- Add conflict detection: warn when two active sessions have pending writes to the same file
- Add click-through navigation from territory entries to the originating conversation message
- Add a persistent territory summary bar visible across all tabs
- Optionally overlay territory indicators in the source view and diff preview tabs


# Implementation Plan: Territory Visualization

This is the concrete delivery plan for the territory visualization feature.

## Core insight

The developer paired with agents is the coordination layer. TermAl's value scales with how many
agents the developer can run concurrently, but that only works if the tool gives them a live picture
of who is changing what. Territory visualization is not a dashboard â€” it is the coordination
surface.

## Goals

- Maintain a server-side aggregated index of all file-level activity across sessions and agents.
- Show the developer a live, at-a-glance territorial map of the project.
- Detect and surface conflicts before they become git merge problems.
- Catch external changes (editor saves, other tools, remote pushes) so the map stays honest.

## Non-goals for v1

- No line-level or hunk-level territory granularity (file-level is enough to start).
- No automatic conflict resolution or agent orchestration.
- No cross-repo territory (single working directory per TermAl instance).
- No persistent territory history across TermAl restarts in v1 (rebuild from session replay later).

## Data model

### Touch event

Every time an agent reads, writes, creates, or deletes a file, the backend records a touch:

```rust
struct Touch {
    file_path: String,
    session_id: String,
    agent: Agent,              // Claude, Codex, Gemini
    action: TouchAction,       // Read, Write, Create, Delete
    lines_added: u32,
    lines_removed: u32,
    message_id: String,        // for click-through to conversation
    timestamp: DateTime<Utc>,
}

enum TouchAction {
    Read,
    Write,
    Create,
    Delete,
}
```

Sources:
- `DiffMessage` on turn completion â†’ `Write` / `Create` / `Delete` with line counts from the diff
- Tool-use events (file_read, file_write, command execution) â†’ `Read` / `Write`
- These are already flowing through the session message stream; the territory index just needs to
  observe them

### File aggregate

The territory index maintains a rolled-up summary per file:

```rust
struct FileTerritory {
    file_path: String,
    touches: Vec<Touch>,        // append-only log
    dominant_agent: Option<Agent>,
    agents_involved: HashSet<Agent>,
    sessions_involved: HashSet<String>,
    contested: bool,            // true if multiple agents have written
    total_writes: u32,
    total_lines_changed: u32,
    last_write: DateTime<Utc>,
    last_read: DateTime<Utc>,
    external_change_detected: bool,
}
```

Rules:
- `dominant_agent` = the agent with the most total `lines_changed` (writes only, reads don't count
  for dominance)
- `contested` = true when two or more distinct agents have at least one `Write` / `Create` / `Delete`
  on the same file
- `external_change_detected` = true when the git poll finds changes not attributable to any session

### Directory rollup

Aggregate file-level data upward into directories so the tree view can show territory at any depth:

```rust
struct DirectoryTerritory {
    dir_path: String,
    dominant_agent: Option<Agent>,
    contested: bool,
    file_count: u32,             // files with any touches under this dir
    contested_file_count: u32,
    agents_involved: HashSet<Agent>,
}
```

This is computed on demand from the file index, not stored separately.

## Git supplementation

The territory map is only useful if it is honest. Agent-tracked touches cover TermAl activity, but
the developer also edits files in their editor, runs scripts, pulls from remote, etc. A periodic
git poll fills that gap.

### Working tree poll

A background task runs on a configurable interval (default: 5 seconds):

1. Run `git status --porcelain` to get the list of modified, added, and deleted files in the
   working tree.
2. For each changed file, check whether the territory index already has a recent touch that explains
   the change (i.e., a TermAl session wrote it within the last poll interval).
3. Any file that changed but has no matching TermAl touch â†’ mark as `external_change_detected` and
   record an `External` touch with no session or agent attribution.
4. Files that were previously marked external but are no longer in `git status` output â†’ clear the
   external flag (the change was committed or reverted).

### Remote poll

A separate, less frequent background task (default: 60 seconds, configurable):

1. Run `git fetch --quiet` to update remote tracking refs.
2. Run `git rev-list --count HEAD..@{upstream}` to check if upstream has new commits.
3. If upstream has diverged, optionally run `git diff --name-only HEAD...@{upstream}` to get the
   list of files that would change on pull.
4. Surface these as `upstream` territory entries â€” files the remote has changed that the developer
   hasn't pulled yet.

This does NOT auto-pull. It just makes the territory map aware that the ground has shifted.

### Git poll constraints

- Both polls run in a dedicated background thread, not on the main tokio runtime, to avoid blocking
  async work.
- Poll intervals should be configurable through the settings API.
- The working tree poll should debounce: if a TermAl agent is actively writing (a turn is in
  progress), skip the poll or suppress external attribution for files the active session is known to
  be editing.
- The remote poll should be opt-in or off by default if the repo has no configured upstream.

## Conflict detection

Contested files are the highest-signal output of the territory system. The backend should
proactively detect and categorize conflicts:

**Level 1 â€” File-level contest:**
Two or more agents have written to the same file. Low urgency; this is information, not necessarily
a problem.

**Level 2 â€” Active collision:**
Two sessions with active (running) turns are both writing to the same file right now. Higher
urgency; one of them is likely about to create a merge conflict.

**Level 3 â€” External desync:**
An agent wrote a file, and then an external change was detected on the same file before the agent's
changes were committed. The agent's mental model of that file is now stale.

Each conflict level should surface differently in the UI (color intensity, icon, notification).

## API

### Territory snapshot

`GET /api/territory`

Returns the full territory map:

```json
{
  "files": [
    {
      "filePath": "src/main.rs",
      "dominantAgent": "Claude",
      "agentsInvolved": ["Claude", "Codex"],
      "sessionsInvolved": ["session-1", "session-4"],
      "contested": true,
      "totalWrites": 12,
      "totalLinesChanged": 347,
      "lastWrite": "2026-03-10T14:22:00Z",
      "lastRead": "2026-03-10T14:25:00Z",
      "externalChangeDetected": false,
      "conflictLevel": 1
    }
  ],
  "conflicts": [
    {
      "filePath": "src/main.rs",
      "level": 1,
      "agents": ["Claude", "Codex"],
      "sessions": ["session-1", "session-4"],
      "description": "Both Claude and Codex have written to this file"
    }
  ],
  "summary": {
    "totalTrackedFiles": 23,
    "byAgent": {
      "Claude": { "files": 8, "linesChanged": 412 },
      "Codex": { "files": 17, "linesChanged": 891 }
    },
    "contestedFiles": 2,
    "externalChanges": 1,
    "activeConflicts": 0
  },
  "gitStatus": {
    "upstreamBehind": 3,
    "upstreamFiles": ["README.md", "Cargo.toml", "src/lib.rs"]
  }
}
```

### Territory for a single file

`GET /api/territory/{filePath}`

Returns the full touch log for one file, including the click-through `messageId` for each touch.
Useful for the detail drill-down.

### Territory SSE

Territory updates should piggyback on the existing `/api/events` SSE stream. When the territory
index changes (new touch, conflict detected, external change found), include a territory delta in
the next SSE snapshot so the frontend stays live without polling.

## UI

### Territory tab

A new workspace tab type:

```ts
type WorkspaceTab =
  | // ... existing types
  | { id: string; kind: "territory" };
```

The tab renders a collapsible project tree with:
- Agent color indicators per file and directory (solid = one agent, striped = contested)
- Recency decay: bright for recent activity, fading over time
- Inline metrics: lines changed, write count
- Conflict badges at each level
- External change indicators
- Click any file â†’ opens the most recent conversation message where it was changed

### Summary bar

Always visible across all tabs (in the status area or header):

`Claude: 8 files (412 lines) Â· Codex: 17 files (891 lines) Â· 2 contested Â· 1 external`

Clicking the summary bar opens the territory tab. Conflict counts should pulse or highlight when a
new conflict is detected.

### Heatmap mode

A toggle in the territory tab that reranks the tree by activity intensity instead of alphabetical
path order. Files with the most cross-agent churn float to the top. Useful for spotting hotspots
when the project tree is large.

### Conflict notifications

When a Level 2 (active collision) or Level 3 (external desync) conflict is detected, surface a
non-blocking toast notification so the developer sees it even if they are not looking at the
territory tab.

## Implementation phases

### Phase 1: touch tracking and server index

- Add the `Touch` and `FileTerritory` structs to backend state.
- Hook into `dispatch_turn()` completion to record touches from `DiffMessage` events.
- Hook into tool-use events for read tracking.
- Add `GET /api/territory` returning the snapshot.
- Add territory deltas to the SSE stream.

### Phase 2: territory tab and summary bar

- Add `territory` as a `WorkspaceTab` kind.
- Render the project tree with agent colors and recency decay.
- Add the persistent summary bar.
- Add click-through from territory entries to conversation messages.

### Phase 3: git supplementation

- Add the working tree poll background task.
- Add external change detection and attribution.
- Add the remote poll background task (opt-in).
- Surface upstream divergence in the territory snapshot and UI.

### Phase 4: conflict detection and notifications

- Implement the three conflict levels.
- Add conflict badges to the territory tree.
- Add toast notifications for Level 2 and Level 3 conflicts.
- Add heatmap mode.

## Testing plan

Backend:
- Touch recording from a simulated turn with diff messages
- File aggregate computation (dominant agent, contested flag, line counts)
- Directory rollup correctness
- External change detection against a mock `git status` output
- Conflict level classification
- Territory snapshot API round-trip

Frontend:
- Territory tab renders file tree with correct agent colors
- Summary bar updates live from SSE deltas
- Recency decay visual correctness (mock timestamps)
- Click-through opens the right session and message
- Heatmap mode reorders by activity
- Conflict toast appears on Level 2+ detection

Integration:
- Two concurrent sessions (Claude + Codex) editing different files â†’ no conflicts, clean territory
- Two concurrent sessions editing the same file â†’ contested flag, Level 1 conflict
- External file edit between agent turns â†’ external change detected, Level 3 if agent wrote it
- Git fetch reveals upstream changes â†’ upstream files surface in territory

## Acceptance criteria

- The territory tab shows a project tree annotated with which agent(s) touched each file.
- The summary bar is visible across all tabs and shows live agent activity counts.
- Contested files are visually distinct from single-agent files.
- External changes (edits outside TermAl) are detected and shown within the poll interval.
- Clicking a file in the territory view navigates to the conversation message that last changed it.
- Conflict notifications surface without requiring the developer to check the territory tab.

