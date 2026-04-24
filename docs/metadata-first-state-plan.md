# Metadata-First State Snapshot Plan

## Goal

Stop sending full conversation transcripts through global state.

`/api/state` and SSE `state` events should describe the app shell and session
summaries only. Full message history should be fetched and reconciled by the
session view through `GET /api/sessions/{id}` and live session deltas.

This is the structural fix for prompt stutter caused by main-thread JSON parsing
of large full-state payloads while Codex or Claude sessions are active.

## Current Problem

The current contract treats `StateResponse` as a full app snapshot:

- `docs/architecture.md` documents `/api/state` and SSE `state` as full state.
- `src/wire.rs::StateResponse.sessions` is `Vec<Session>`.
- `src/wire.rs::Session` includes `messages: Vec<Message>`.
- `src/state_accessors.rs::snapshot_from_inner_with_agent_readiness` clones
  every visible session into `StateResponse`.
- `ui/src/api.ts::StateResponse.sessions` is typed as `Session[]`.

That means every accepted `/api/state` response and every adopted SSE `state`
event can force the browser to parse every historical message for every visible
session. Recent profiling showed the dominant cost as `JSON.parse`, often
hundreds of milliseconds, before React reconciliation even starts.

## Target Contract

Global state becomes metadata-first.

`StateResponse` should contain:

- revision
- serverInstanceId
- codex global state
- agent readiness
- preferences
- projects
- orchestrator instances
- workspace layout summaries
- session summaries

`StateResponse` should not contain:

- full `messages`
- transcript-heavy command output bodies
- large diff/message payloads
- rendered-message cache data

Full session data remains available through:

- `GET /api/sessions/{id}` -> `SessionResponse { revision, session }`
- session-scoped SSE deltas for live updates
- create/fork/send responses that need to return a concrete session or message

This includes local and remote sessions. A local proxy for a remote session must
not depend on remote `/api/state` for transcript repair after this refactor; it
must hydrate the targeted remote session and then localize the returned session
id/project/session references before writing the local proxy record.

## Wire Model

Introduce an explicit summary type instead of relying on "empty messages" as the
long-term API shape.

Backend target:

```rust
struct StateSessionSummary {
    id: String,
    name: String,
    emoji: String,
    agent: Agent,
    workdir: String,
    project_id: Option<String>,
    model: String,
    model_options: Vec<SessionModelOption>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
    external_session_id: Option<String>,
    agent_commands_revision: u64,
    codex_thread_state: Option<CodexThreadState>,
    status: SessionStatus,
    preview: String,
    pending_prompts: Vec<PendingPrompt>,
    session_mutation_stamp: Option<u64>,
    messages_loaded: bool, // always false in StateResponse summaries
    message_count: usize,
}

struct StateResponse {
    revision: u64,
    server_instance_id: String,
    codex: CodexState,
    agent_readiness: Vec<AgentReadiness>,
    preferences: AppPreferences,
    projects: Vec<Project>,
    orchestrators: Vec<OrchestratorInstance>,
    workspaces: Vec<WorkspaceLayoutSummary>,
    sessions: Vec<StateSessionSummary>,
}
```

Frontend target:

```ts
export type StateSessionSummary = Omit<Session, "messages"> & {
  messagesLoaded: false;
  messageCount: number;
};

export type StateResponse = {
  revision: number;
  serverInstanceId: string;
  codex: CodexState;
  agentReadiness: AgentReadiness[];
  preferences: AppPreferences;
  projects: Project[];
  orchestrators: OrchestratorInstance[];
  workspaces: WorkspaceLayoutSummary[];
  sessions: StateSessionSummary[];
};
```

During migration, a temporary frontend-only adapter may convert
`StateSessionSummary` into the legacy `Session` shape with `messages: []` and
`messagesLoaded: false` at the UI boundary. The backend should not ship a new
long-lived "summary but typed as full Session" contract. Any compatibility
adapter must be documented as temporary and removed once pane, tab, composer,
and orchestrator surfaces consume summaries directly.

## Contract Precisions

These details close ambiguities in the wire shape that would otherwise be
decided ad hoc during implementation.

### Field semantics

- **`revision` on `StateResponse`**: the global `StateInner::revision` at
  snapshot time. Clients track this as the delta-ordering key.
- **`revision` on `SessionResponse`**: the global revision at read time — not a
  per-session counter. Any hydration response for which
  `SessionResponse.revision < last_adopted_state_revision` is stale and MUST be
  rejected by the client. A delta with `sessionId == id` arriving after an
  accepted hydration is applied on top.
- **`sessionMutationStamp`**: the authoritative per-session ordering key, bumped
  on any session mutation (message add/edit/replace/delete plus metadata
  changes that the UI cares about). Clients use the pair
  `(sessionMutationStamp, messageCount)` as the cheap gap-detection tuple: if
  the stamp matches, `messageCount` must match.
- **`messageCount: u32`** (not `usize`): the count of messages
  `GET /api/sessions/{id}` would return at snapshot time, after hidden-message
  filtering. Not monotone — compaction can reduce it. Used by the UI as a
  loading-skeleton height estimate and as the count half of the gap-detection
  tuple. `u32` is picked deliberately; `usize` has no portable wire width.
- **`messagesLoaded`**: always `false` in `StateSessionSummary` and is wire
  redundant — the type itself is the discriminant. Keep it on the wire only
  during the transitional adapter window (Phase 2 → Phase 3). It MUST be removed
  from the wire type at Phase 5; the frontend derives `messagesLoaded` from its
  local store thereafter.
- **`preview: String`**: bounded by the existing preview helper to a short
  snippet (<= 240 bytes). Summaries MUST not embed full last-message text here.
- **`pending_prompts`**: see "Bounded summary fields" below. This field does
  NOT carry attachment payloads in summaries.

### Discriminated union (frontend)

The existing `Session.messagesLoaded?: boolean | null | undefined` is too loose
to make `StateSessionSummary` a real discriminant. Phase 3 tightens:

```ts
// Hydrated transcripts: messagesLoaded is required-true.
export type Session = {
  // ...existing fields...
  messages: Message[];
  messagesLoaded: true;
};

export type StateSessionSummary = Omit<Session, "messages" | "messagesLoaded"> & {
  messagesLoaded: false;
  messageCount: number;
};

// Use an explicit union where a reader accepts either shape:
export type AnySession = Session | StateSessionSummary;
```

Narrowing from `AnySession` to `Session` MUST go through a
`messagesLoaded === true` check. No `as Session` casts are permitted. Phase 5
enforces this at review.

### Hydration state machine

Frontend tracks per-session hydration status explicitly, stored on the session
record (visible to `useSyncExternalStore` consumers):

```
hydrationStatus: "idle" | "loading" | "hydrated" | "error"
```

- `idle`: the session is a summary that has never been hydrated in this tab.
- `loading`: a `fetchSession` is in-flight. Includes the `serverInstanceId`
  captured at send-time. Responses returning after the local
  `serverInstanceId` has changed are rejected.
- `hydrated`: transcript is present, messagesLoaded === true.
- `error(kind)`: last fetch failed. `kind` distinguishes 404 (gone),
  `transient` (network/5xx), and `stale` (revision too old). UI shows an
  inline retry affordance for transient/stale; 404 triggers session removal.

Transitions:
- Session activation with status `idle` → `loading` (fires `fetchSession`).
- `fetchSession` resolves and revision gate passes → `hydrated`.
- `fetchSession` resolves and revision gate fails → refetch (at most once),
  then `error("stale")` if it keeps failing.
- Rapid activation toggles coalesce: a second activation while `loading` does
  NOT fire a second fetch.
- Two panes on the same session share `loading` / `hydrated` via the store.
- `serverInstanceId` change clears all `hydrating`/`hydrated` status for
  sessions that aren't present in the new summary list.

### Bounded summary fields

Summary fields that would otherwise carry arbitrary user input are bounded so
payload size stays proportional to session count:

- `pending_prompts` on `StateSessionSummary` ships a lightweight shape:
  ```rust
  struct PendingPromptSummary {
      id: String,
      preview: String, // capped to PENDING_PROMPT_PREVIEW_MAX_BYTES (e.g. 240)
      has_attachments: bool,
      attachment_count: u32,
      created_at: String,
  }
  ```
  Full `PendingPrompt` (with `expanded_text` and `attachments`) remains
  available via `GET /api/sessions/{id}`. Rationale: an attached-image queue
  of 10 prompts would otherwise re-introduce the exact payload problem this
  plan solves.
- `preview` is bounded (see above).
- `codex_thread_state` is intentionally a small enum; adding payload-carrying
  variants requires revisiting whether the field belongs on the summary.

### Delta arrival for un-hydrated sessions

A session-scoped `DeltaEvent::*` targeting a session with
`messagesLoaded === false` MUST be handled as follows by the frontend:

1. Never apply the message-content payload to the empty transcript (doing so
   creates a hole that later hydration cannot fix cleanly).
2. Never trigger `needsResync` / `/api/state` refetch on the mismatch (that
   causes an infinite resync loop on every background-streaming session).
3. Advance summary-relevant metadata locally from whatever the delta carries:
   `sessionMutationStamp`, `messageCount` (infer or update from delta shape),
   `preview`, `status` if the delta exposes them.
4. The delta is otherwise dropped. The next activation of the session runs
   hydration, which returns the current transcript including the dropped
   delta's effect.

Deltas MAY extend their shapes to include `preview`, `sessionMutationStamp`,
`messageCount` fields so that step 3 is always possible. Phase 1's delta audit
commits to this contract (see below).

### Endpoint error semantics

- `GET /api/sessions/{id}` returns `200 SessionResponse` on success, `404
  ApiError` when the id is unknown (dropped on the server between summary and
  fetch), `502 ApiError` when a remote proxy's upstream is unreachable, and
  `500 ApiError` for unexpected failure. The frontend evicts the session on
  `404` (the summary is stale), retries on `502`/`500` with backoff, and
  surfaces `error(kind)` in the pane for display.
- Concurrent `fetchSession(id)` calls for the same `id` are coalesced client-
  side; at most 4 distinct hydration fetches run in parallel.

### Version negotiation

`HealthResponse` gains `supportsMetadataFirstState: true`. Remote proxies
(`src/remote_routes.rs`, `RemoteRegistry`) inspect the bit during
`start_event_bridge`:

- Remote reports `true` → the local proxy consumes summary-only state and
  issues targeted `GET /api/sessions/{id}` to the remote for active panes.
- Remote reports `false` or absent → the local proxy treats the remote as
  legacy: accepts full-session state from that remote and adapts to summaries
  internally before forwarding to local UI. This path is marked for removal at
  the same deadline as the transitional adapter (see Rollout Strategy).

## Implementation Phases

### Phase 0: Baseline and Inventory

Before any production code changes, capture the "before" picture so subsequent
phases can verify they improved it and didn't regress invariants that weren't
previously tested.

Work:

- **Perf baseline.** Run `scripts/perf/prompt-responsiveness-smoke.js` and
  `scripts/perf/active-codex-typing.js` against a worst-case fixture
  (10 sessions × 500 messages × 2 KB each, one session actively streaming
  Codex). Record: average next-frame delay, worst next-frame delay, SSE
  `state` event parse time (`performance.mark` around `JSON.parse` in the
  SSE handler), total state-event payload size in bytes. Commit the numbers
  as `docs/metadata-first-baseline.json` so future perf ratchets reference
  an artifact, not memory.
- **`commit_locked` caller inventory.** Grep every call site under `src/`
  and produce a checklist artifact (`docs/metadata-first-callers.md`, or
  a comment table embedded in this plan) with columns:
  `(file:line, helper, mutates messages? Y/N, publishes delta? Y/N)`.
  Known sites that must appear:
  - `src/session_interaction.rs::set_approval_decision_on_record`,
    `set_user_input_request_state_on_record`,
    `set_mcp_elicitation_request_state_on_record`,
    `set_codex_app_request_state_on_record` — called from
    `src/codex_submissions.rs:~280, 373, 476, 571`. These mutate messages
    in place with no matching delta today.
  - `src/messages.rs::cancel_pending_interaction_messages` — called from
    `src/session_lifecycle.rs:~336` and `src/turn_lifecycle.rs:~393`.
    Rewrites existing messages without a per-message delta.
  - `src/sse_broadcast.rs::record_active_turn_file_changes` path
    (~line 368) appends `FileChangesMessage` via
    `push_active_turn_file_changes_on_record` under plain `commit_locked`.
  - `src/turn_lifecycle.rs:97, 187, 234, 302, 417, 589` — turn lifecycle
    message pushes; audit each for delta coverage.
  - Orchestrator transitions that push stopped-session cleanup messages
    (`src/orchestrator_transitions.rs`).
  All `commit_locked` callers not on the above list still need to be
  enumerated; the list above is a seed, not a complete set.
- **`StateResponse`-returning endpoint inventory.** Enumerate every HTTP
  handler in `src/main.rs` + module routers whose response type is
  `StateResponse`. Known mutation endpoints that return it today:
  `sendMessage`, `submitApproval`, `submitUserInput`,
  `submitMcpElicitation`, `submitCodexAppRequest`, `updateSessionSettings`,
  `renameSession`, `killSession`, `stopSession`, `cancelQueuedPrompt`,
  `archiveCodexThread`, `unarchiveCodexThread`, `compactCodexThread`,
  `rollbackCodexThread`, `refreshSessionModelOptions`,
  `pauseOrchestratorInstance`, `resumeOrchestratorInstance`,
  `stopOrchestratorInstance`, `updateAppSettings`, `deleteProject`. For
  each, decide in Phase 3 between "change response shape to carry
  `SessionResponse`" and "prove the matching SSE delta always lands before
  the HTTP response resolves." Capture the decision per-endpoint in the
  checklist.
- **`session.messages` reader inventory (frontend).** List every file that
  reads `session.messages` outside the transcript virtualizer. Seed:
  - `ui/src/app-utils.ts` — `buildSessionConversationSignature`,
    `collectCandidateSourcePaths`, `findLastUserPrompt`.
  - `ui/src/session-find.ts:~128` — search index.
  - `ui/src/session-store.ts` — `resolvePromptHistory` (composer Up-arrow
    recall), `buildComposerSessionSnapshot`.
  - `ui/src/live-updates.ts` — `hasAssistantActivitySinceCurrentTurnBoundary`.
  - `ui/src/SessionPaneView.tsx` — `commandMessages`, `diffMessages`
    projections.
  - `ui/src/panels/AgentSessionPanel.tsx` and
    `ui/src/panels/VirtualizedConversationMessageList.tsx` — direct reads.
  - `ui/src/control-surface-state.ts::mergeOrchestratorDeltaSessions`.
  Phase 3 visits each and either refactors to summary-aware behavior or
  documents the "requires hydrated session" invariant.
- **Fixture inventory.** Grep test files for inline `messages: [` and
  record a count; target is a mechanical transform scope for Phase 5.

Exit criteria:

- Baseline artifact committed and linked from this plan.
- Caller inventory covers every `commit_locked` call site with explicit
  delta-coverage answers.
- `StateResponse`-returning endpoint checklist has a per-endpoint decision.
- `session.messages` reader inventory is enumerated in this plan or a sibling
  file.

### Phase 1: Prove Delta Coverage

Using Phase 0's inventory, close the delta-coverage gaps before stripping
transcripts from global state.

Work:

- For every row in the `commit_locked` inventory where "mutates messages"
  is Y but "publishes delta" is N, convert the call site to
  `commit_persisted_delta_locked` plus an explicit delta, or publish a
  companion delta alongside the existing `commit_locked`.
- Introduce new `DeltaEvent` variants when an existing variant does not fit
  an audited mutation. Candidates likely required:
  - `DeltaEvent::MessageReplaced { session_id, message_id, message, revision, session_mutation_stamp }`
    for in-place edits (approval decision, interaction state transitions).
  - `DeltaEvent::MessagesCancelled { session_id, message_ids, revision, session_mutation_stamp }`
    for `cancel_pending_interaction_messages`.
  - Or a narrower `TextReplace` generalization — pick one and document it.
- Extend each session-scoped `DeltaEvent` variant that today carries only
  the content change to also carry (at least) `session_mutation_stamp` plus
  the fields the summary needs to keep fresh without hydration
  (`message_count`, and `preview` when a new message becomes the latest).
  Rationale: frontend step 3 of "Delta arrival for un-hydrated sessions"
  (above) depends on this.
- Convert transcript-bearing delta payloads that are not truly message
  deltas to summary payloads or id-only invalidation. In particular,
  `DeltaEvent::OrchestratorsUpdated { sessions: Vec<Session> }` and
  `DeltaEvent::SessionCreated { session: Session }` currently carry full
  sessions and must become summary-only or id-only + targeted hydration.
- Confirm non-message session metadata changes remain represented in
  summaries.
- Confirm session creation still returns enough data for the new pane to
  render immediately (create/fork responses keep the full `SessionResponse`
  shape — they are not `StateResponse` mutators).
- Decide the per-endpoint answer for every `StateResponse`-returning
  mutation endpoint from the Phase 0 inventory. If an endpoint's SSE delta
  does not always land before the HTTP response resolves (send-message is
  the canonical example: the UI relies on the send response to show the
  user's own prompt before the first assistant chunk), change the response
  shape to `SessionResponse` or a narrower `MessageAppendedResponse`.

Exit criteria:

- No message correctness path depends on `StateResponse.sessions[*].messages`.
- Every row in the caller inventory has a passing test that subscribes to
  `state.subscribe_events()`, drives the mutation path, and asserts a
  matching `DeltaEvent` (not only a `State` event) is observed.
- SSE event shapes other than explicit session/message deltas cannot carry
  full transcripts: `SessionCreated`, `OrchestratorsUpdated`, and any new
  variant introduced in this phase carry summaries or ids, not full
  `Session` objects.
- Every `StateResponse`-returning mutation endpoint has either (a) been
  converted to a session-bearing response, or (b) a regression test that
  proves the matching SSE delta lands before the HTTP response resolves.

### Phase 2: Add Backend Summary Snapshot

Work:

- Add `StateSessionSummary` in `src/wire.rs`.
- Add `state_session_summary_from_record` beside
  `wire_session_from_record`.
- Change `snapshot_from_inner_with_agent_readiness` to emit summaries in
  `StateResponse`.
- Keep `SessionResponse` unchanged and transcript-bearing.
- Keep persistence unchanged. SQLite and in-memory `SessionRecord` still store
  full sessions.
- Update SSE fallback payload builders to emit summary state.
- Verify remote-proxy paths that consume `StateResponse` do not require
  transcript messages from state snapshots.
- Change `DeltaEvent::OrchestratorsUpdated` so referenced sessions are
  represented as `StateSessionSummary[]` or as ids that force targeted
  hydration. Do not keep `Vec<Session>` on this event after state snapshots are
  metadata-first.
- Add backend helpers for targeted remote session hydration. Remote snapshot
  sync may create/update summary proxy records, but active transcript repair
  must request the specific remote session and localize the returned
  `SessionResponse`.

Exit criteria:

- `/api/state` response bodies do not include a `messages` field for sessions,
  or include only the temporary adapter-compatible empty `messages: []` with
  `messagesLoaded: false` if the migration phase requires it.
- `GET /api/sessions/{id}` still returns the full transcript.
- SSE `state` events have payload size proportional to session count, not total
  message count.
- Orchestrator update deltas also have payload size proportional to referenced
  session count, not referenced transcript size.

### Phase 3: Frontend Summary Adoption

Work:

- Add `StateSessionSummary` to `ui/src/api.ts` and tighten `Session` so
  `messagesLoaded: true` is required (see Contract Precisions → Discriminated
  union).
- Add a summary-to-store merge helper that preserves already-hydrated session
  messages. This is the enforcement point for "adoptState never replaces a
  hydrated active session transcript with an empty summary." Sketch:
  ```ts
  function mergeSummaryIntoPrevious(
    previous: AnySession | undefined,
    next: StateSessionSummary,
  ): AnySession {
    if (previous && previous.messagesLoaded) {
      return { ...previous, ...next, messages: previous.messages,
               messagesLoaded: true };
    }
    return next;
  }
  ```
- Change `reconcileSessions(prev, next, options)` to accept
  `next: AnySession[]` and return `AnySession[]`. New contract:
  - When `next[i].messagesLoaded === false`, merge non-transcript fields from
    `next[i]` into the matching `prev[i]` but preserve `prev[i].messages`
    verbatim; final object is a hydrated `Session`.
  - When `next[i].messagesLoaded === true`, follow the existing reconcile
    logic (mutation-stamp fast path, id-aware message reconcile).
  - `sameSessionSummary` short-circuit applies when `next[i]` is a summary and
    the summary fields match `prev[i]`'s corresponding fields.
- Implement the hydration state machine documented in Contract Precisions:
  store `hydrationStatus` on the session record, coalesce rapid activations,
  share status across panes via the session store, capture
  `serverInstanceId` at fetch send and reject on mismatch at receive, evict
  on 404.
- Keep `adoptFetchedSession` as the only full-transcript adoption path
  outside create/fork/send responses. Update its stale-check to use
  `currentSession.messagesLoaded !== true` instead of
  `currentSession.messages.length === 0`.
- Enforce the "delta arrival for un-hydrated sessions" policy from Contract
  Precisions in `ui/src/live-updates.ts::applyDeltaToSessions`: for a session
  with `messagesLoaded === false`, update summary-relevant metadata from the
  delta and drop the message-content payload — never call `needsResync`.
- Visit every entry in the Phase 0 `session.messages` reader inventory and
  either refactor to summary-aware behavior or gate on
  `session.messagesLoaded === true`. Expected per-reader resolution:
  - `ui/src/app-utils.ts::buildSessionConversationSignature` — signature
    reads are only meaningful for hydrated sessions; gate callers.
  - `ui/src/app-utils.ts::collectCandidateSourcePaths`,
    `findLastUserPrompt` — gate on hydrated.
  - `ui/src/session-find.ts` search index — disable (or show a "hydrate to
    search" affordance) for summary-only sessions. Document that
    cross-session client-side search is out of scope.
  - `ui/src/session-store.ts::resolvePromptHistory`,
    `buildComposerSessionSnapshot` — either carry a bounded
    `recentPromptHistory` on `StateSessionSummary` or trigger hydration on
    composer focus. Default: trigger hydration on composer focus so the
    summary stays cheap.
  - `ui/src/live-updates.ts::hasAssistantActivitySinceCurrentTurnBoundary` —
    this is a pre-send staleness check; only relevant when the session is
    hydrated, gate accordingly.
  - `ui/src/SessionPaneView.tsx` `commandMessages` / `diffMessages` — these
    projections must return `[]` for summary-only sessions and show a loading
    affordance; the Commands and Diffs tabs render a skeleton keyed on
    `messageCount`.
  - `ui/src/panels/AgentSessionPanel.tsx` and
    `ui/src/panels/VirtualizedConversationMessageList.tsx` — show a
    loading skeleton (height estimated from `messageCount`) for
    `messagesLoaded === false && messageCount > 0`. Keep the existing empty
    state for `messagesLoaded === false && messageCount === 0`.
  - `ui/src/control-surface-state.ts::mergeOrchestratorDeltaSessions` — must
    accept `StateSessionSummary[]` after Phase 2 (orchestrator delta shape
    change).
- Add a failure state to the session pane for `hydrationStatus === "error"`
  that renders an inline retry button and the error kind.

Exit criteria:

- Opening a summary-only session triggers exactly one `fetchSession`. Rapid
  open → close → open within 100ms triggers at most one fetch. Reopening an
  already-hydrated session within the freshness window does not refetch.
- Two panes opened to the same session share a single in-flight fetch and a
  single resulting transcript.
- A summary state event for an already hydrated active session preserves
  local messages.
- Deltas arriving for an un-hydrated session update summary metadata and do
  NOT trigger `needsResync` / `/api/state` resync storms.
- Hidden sessions do not keep transcript arrays alive just because they
  appear in global state. Closing a session pane (or deactivating it beyond
  a retention threshold) drops the transcript and restores `messagesLoaded:
  false` until re-open.
- Later summary snapshots preserve transcript-derived state (prompt history,
  search index, last-message author) for already-hydrated sessions.
- `serverInstanceId` change invalidates `hydratingSessionIdsRef` and
  `hydratedSessionIdsRef` before applying the new summary list.
- A `fetchSession` 404 evicts the session locally; 5xx/network failures show
  an inline retry affordance.

### Phase 4: Recovery and Remote Semantics

Work:

- Revisit delta-gap handling. A gap should still fetch `/api/state` for global
  metadata, but any active/hydrated session that needs transcript repair should
  fetch `GET /api/sessions/{id}`.
- Ensure server restart adoption updates `revision` and `serverInstanceId`
  atomically with summary state.
- Ensure remote SSE fallback and remote state sync preserve session summaries
  and do not silently drop remote transcript updates.
- Add required targeted hydration for active remote panes after a gap,
  reconnect, or summary-only remote snapshot. The local server must proxy
  `GET /api/sessions/{remote_session_id}` to the remote, localize the returned
  session, and merge it into the matching local proxy record.
- Preserve remote revision gating: a successful `/api/state` metadata fetch can
  repair remote summaries, but active transcript repair is complete only after
  the targeted session hydration succeeds or live deltas catch up.

Exit criteria:

- A state snapshot can repair global metadata after a reconnect without parsing
  transcripts.
- Active session transcript repair is targeted by session id.
- Remote/local behavior remains consistent for visible sessions.
- Remote active panes recover transcripts without broad remote `/api/state`
  carrying messages.

### Phase 5: Remove Transitional Adapters

Work:

- Replace remaining `StateResponse.sessions: Session[]` assumptions with
  `StateSessionSummary[]`.
- Remove the temporary `messages: []` compatibility adapter. No production
  path constructs a summary with a `messages` field after this phase.
- Remove `messagesLoaded` from `StateSessionSummary` on the wire (see
  Contract Precisions — the bit is redundant with the type itself). Frontend
  continues to track `messagesLoaded` on its local `Session` / store records.
- Drop the legacy-remote compatibility branch in the remote-proxy bridge
  (the `supportsMetadataFirstState === false` path). All remotes in scope
  are expected to speak the new protocol by this deadline.
- Make TypeScript prevent direct message access from global state summaries:
  - `StateResponse.sessions: StateSessionSummary[]` (no union with
    `Session`).
  - `Session` requires `messagesLoaded: true`; narrowing from `AnySession`
    is the only path to reading `messages`.
  - Add a `eslint-no-restricted-syntax` rule (or equivalent) banning
    `as Session` and `as unknown as Session` casts in non-test code.
    Reviewer discipline enforces exceptions with an inline
    `// eslint-disable-next-line ...` and a justification comment.
  - Add a `expectTypeOf<StateSessionSummary>().not.toHaveProperty("messages")`
    compile-time test so a later accidental widening fails CI.
- Mechanically update test fixtures:
  - Every inline `Session { ... messages: [...] }` fixture gains
    `messagesLoaded: true` (matches the narrowed `Session` type).
  - Every `StateResponse { ... sessions: [...] }` fixture maps through a
    new `toSummary()` helper, or uses `StateSessionSummary`-shaped literals
    directly.
  - Centralize a `hydrateSessionForTest(id, messages)` helper. Tests that
    need transcript data call it after an `adoptState` step; they do not
    inline transcripts into `StateResponse` fixtures.
  - An `assertNoTranscriptInState(state)` helper runs in a top-level
    `describe.each` guard for every `adoptState`-producing test. A fixture
    that sneaks `messages` into a state response fails this guard.
  - Rust test helpers: add an equivalent assertion in `src/tests/mod.rs`
    that every `StateResponse` built in test scaffolding has no
    non-empty `messages` on any session.
  - Specific Rust test files to audit and update:
    `src/tests/persist.rs`, `src/tests/remote.rs`,
    `src/tests/http_routes.rs` (if present), `src/tests/mod.rs` helpers.
  - Specific TypeScript test files: `ui/src/mockData.ts`,
    `ui/src/App.live-state.*.test.tsx`, `ui/src/session-reconcile.test.ts`,
    `ui/src/session-store.test.ts`, plus any file matched by the
    `messages: [` grep from Phase 0's fixture inventory.
  - Expected blast radius: ~200 fixture sites. Budget a day for the
    mechanical transform; supply a codemod script if the scope is > 100.

Exit criteria:

- `StateResponse` cannot carry full session messages at the type level.
- UI code that needs messages goes through session-store hydration or
  `SessionResponse`; no `as Session` casts in production code.
- Test fixtures either target the new contract or are explicitly annotated
  as legacy-coverage tests.
- Compile-time test pins the absence of `messages` on `StateSessionSummary`.
- `docs/metadata-first-baseline.json` is compared against the final perf run
  and the delta is recorded in `docs/prompt-responsiveness-refactor-plan.md`.

## Documentation Updates

Update documentation in the same PR/changeset as the implementation.

Required updates:

- `docs/architecture.md`
  - Change `/api/state` from "Full state snapshot" to "metadata-first state
    snapshot".
  - Change SSE `state` docs from "full StateResponse JSON" to
    "metadata-first StateResponse JSON".
  - Document that `GET /api/sessions/{id}` is the full transcript hydration
    route.
  - Document `StateSessionSummary`, `messagesLoaded: false`, and `messageCount`.
  - Update real-time update docs so delta gaps mention targeted session
    hydration when transcript repair is needed.
  - Update `DeltaEvent::OrchestratorsUpdated` docs to show summary/id payloads
    instead of full sessions.
  - Document the local/remote split: remote `/api/state` repairs summaries;
    remote `GET /api/sessions/{id}` repairs one transcript.

- `docs/prompt-responsiveness-refactor-plan.md`
  - Mark the metadata-first state work as the backend/API half of the prompt
    responsiveness refactor.
  - Link to this plan.
  - Update completion criteria to include "global state parse time is
    independent of total transcript size".

- `docs/bugs.md`
  - Move "State snapshots still include full session transcripts on the wire"
    to the fixed preamble once implemented.
  - Remove or update task items that metadata-only snapshot coverage completes.
  - Add any newly discovered follow-up bugs from the audit.

- `docs/test.md`
  - Add testing guidance that global state fixtures should not include message
    transcripts unless the test explicitly covers legacy/full-session paths.
  - Add guidance for targeted session hydration tests.
  - Describe the `assertNoTranscriptInState(state)` helper and the
    `hydrateSessionForTest(id, messages)` helper, and require every
    `adoptState`-producing test to route through the guard.

- `docs/architecture.md` (new subsection)
  - Add a **Session Hydration** subsection documenting the
    `(sessionMutationStamp, messageCount)` gap-detection tuple, 404 / 5xx /
    stale error cases on `GET /api/sessions/{id}`, the client's required
    retry/eviction behavior, the hydration state machine, and the
    delta-on-un-hydrated-session policy.
  - Add a **Version negotiation** note referencing
    `HealthResponse.supportsMetadataFirstState` and the fallback path for
    legacy remotes during the transitional window.
  - Explicitly mark pagination of `GET /api/sessions/{id}` as a non-goal
    for this plan (a long Codex thread still pays a full-transcript parse
    on hydration; a `?offset=` endpoint is a separate project).

- Security review
  - Run the `security-review` lens against the implementation PR. Accepted
    risks are recorded in `docs/bugs.md` under a "Metadata-first rollout"
    note. The review must confirm:
    - 404 handling prevents stuck hydration state and does not leak orphan
      panes.
    - `serverInstanceId` capture-at-send / reject-at-receive prevents stale
      responses from crossing instance boundaries.
    - Remote-sourced session ids are validated against a charset before
      being used in URL construction for the targeted hydration proxy.
    - Client-side coalescing and concurrency cap on `fetchSession`
      prevent hydration storms.

## Test Plan

Backend tests:

- `/api/state` response has no `messages` field on any session. Asserted
  structurally via a `serde_json::Value` walk that fails if any
  `sessions[*].messages` path exists with non-empty content — stronger than
  a payload-size check alone.
- `GET /api/sessions/{id}` returns the full transcript for the same session.
- `GET /api/sessions/{id}` returns `404 ApiError` for unknown ids, including
  the race where a session is deleted between the summary broadcast and the
  hydration fetch.
- SSE initial `state` omits transcript payloads. The SSE-fallback/resnap
  path also emits summary-only; test an SSE reconnect after N message
  mutations and assert the replayed state event carries no transcript.
- `publish_state_locked` summary payload size does not scale with message
  body size. Parametrize the test with 100 sessions × N messages × 2 KB for
  varying N and assert the state-event serialized size is within a constant
  multiple of `N_sessions × MAX_SUMMARY_BYTES`.
- Every row in the Phase 0 `commit_locked` caller inventory: drive the
  mutation, subscribe to `state.subscribe_events()`, assert a matching
  `DeltaEvent` (not only a `State` event) is observed. Concretely covers
  `push_message`, `append_text_delta`, `replace_text_message`,
  `update_command_output`, `update_parallel_agents`,
  `set_approval_decision_on_record`,
  `set_user_input_request_state_on_record`,
  `set_mcp_elicitation_request_state_on_record`,
  `set_codex_app_request_state_on_record`,
  `cancel_pending_interaction_messages`, and the workspace-file-change
  append path.
- Every published session-scoped delta carries
  `session_mutation_stamp: Some(record.mutation_stamp)` matching the wire
  session's stamp. Prevents silent stamp-drop regressions across the 13+
  publish sites.
- Remote-proxy state sync still exposes remote session summaries and does
  not clear the proxy's `message_positions` when the remote payload has no
  transcripts. Test: local proxy has a hydrated remote session, remote
  summary arrives, assert the local proxy keeps its transcript.
- `DeltaEvent::OrchestratorsUpdated` and `DeltaEvent::SessionCreated` do
  not serialize full session transcripts (summary-only or id-only).
- Remote targeted session hydration localizes the returned full session
  and updates only the matching proxy record.
- Apply `DeltaEvent::CodexUpdated` through `apply_remote_delta_event`:
  no state broadcast emitted, `applied_revision_by_remote` advances.
- `note_codex_notice` publish path emits a narrow `CodexUpdated` delta
  with no full-state broadcast (mirrors the existing rate-limits test).
- `shared_codex_rate_limits_publish_codex_delta_without_full_state_snapshot`
  is tightened: after draining the delta, `state_events.try_recv()` is
  asserted to return `Empty`.

Frontend tests:

- App adopts summary-only state and renders session list/tab metadata,
  tab labels, and the pane title bar identically to pre-refactor output
  for a summary-only active session.
- Activating a summary-only session calls `fetchSession` once. Rapid
  open → close → open within 100ms triggers at most one fetch.
- Two panes open to the same summary-only session: only one
  `fetchSession` fires, both panes render the resulting transcript, and
  subsequent deltas apply to both.
- Session switch before hydration completes: the late response for
  session A does not overwrite A's local store entry; returning to A
  uses the cached transcript.
- Server-restart mid-hydration: `serverInstanceId` flips before the
  fetch resolves; the late response is rejected under the new instance
  id, and `hydratingSessionIdsRef` / `hydratedSessionIdsRef` are cleared.
- Hydrated active messages survive later summary snapshots.
- Delta updates apply to hydrated session records without fetching
  `/api/state`.
- Delta arrives for a session with `messagesLoaded: false`: store
  updates `sessionMutationStamp` / `messageCount` / `preview` / `status`
  from the delta payload, transcript remains empty, NO `needsResync`
  fires. Follow-up activation hydrates the correct transcript.
- Rapid delta stream against a summary-only session does not trigger
  any `/api/state` refetch.
- Summary state immediately followed by a delta for the same session:
  delta is applied correctly (ordering preserved regardless of which
  arrives first over SSE).
- Delta-gap recovery fetches `/api/state` for metadata and targeted
  `fetchSession` for currently-mounted panes; other sessions stay
  summary-only until activated.
- Stale summary snapshots are rejected (lower `revision` than current);
  store state and transcript references unchanged by `===` identity, not
  just deep equality.
- Summary snapshots do not clear prompt history, search index, last-
  message-author state, or diff/commands projections for already-
  hydrated sessions.
- Orchestrator update deltas merge session summaries without introducing
  transcript payloads; `mergeOrchestratorDeltaSessions` accepts
  `StateSessionSummary[]` inputs.
- `adoptFetchedSession`'s stale-check uses `messagesLoaded !== true`,
  not `messages.length === 0`.
- Summary-only session transitions Active → Idle mid-turn: UI reflects
  status change without triggering hydration.
- Workspace restore from persistence: 5 sessions in persistence → initial
  `/api/state` carries 5 summaries → tab strip, pane, and workspace
  layout render with no unhydrated-message errors. Opening each
  sequentially triggers exactly one fetch each.
- Summary-only session search / find-in-session: UI renders a loading or
  disabled affordance, not a false-empty result.
- Composer for a summary-only session: disabled until hydrated OR
  triggers hydration on first focus (pick one in Phase 3 and test it).
- Create / fork / send responses hydrate immediately; a subsequent
  summary state does not regress the transcript.
- Hydration 404 evicts the session from the store; hydration 5xx surfaces
  an inline retry affordance.
- Concurrent `fetchSession(id)` calls for the same id coalesce: at most
  one in-flight request per id, at most 4 in-flight across all ids.
- Closed panes evict their transcripts from the store (or after the
  retention threshold): session store's transcript footprint stays
  bounded across rapid pane navigation.

Type-level tests:

- `expectTypeOf<StateSessionSummary>().not.toHaveProperty("messages")`
  (Phase 5).
- A `// @ts-expect-error` annotated test that casts
  `StateSessionSummary` to `Session` without narrowing fails to
  compile.

Performance checks:

- Re-run Phase 0's baseline harness after Phase 3 and after Phase 5;
  compare against `docs/metadata-first-baseline.json`. Thresholds
  (targets, not hard gates — tighten based on the actual before number):
  - Average next-frame delay while Codex streams: ≤ 10 ms (down from
    the baseline; record the actual "before" in the artifact).
  - Worst next-frame delay over a 10-session × 500-message × 2 KB
    workspace: ≤ 30 ms.
  - SSE `state` event `JSON.parse` time: ≤ 20 ms for the same workspace.
  - Total `/api/state` response body size: bounded by
    `O(N_sessions × MAX_SUMMARY_BYTES)`; verify payload size stays
    under a concrete byte budget for the 10-session fixture (target:
    < 40 KB including workspace layout and agent readiness).
- Verify prompt typing stays responsive while active agents emit deltas
  (unchanged goal; measured via the baseline harness).

Fixture discipline:

- Every new `adoptState`-producing test runs through
  `assertNoTranscriptInState(state)`. This is a non-optional guard; a
  fixture that includes full messages in a state response fails the
  test suite even if the test doesn't assert on transcripts.
- New tests that need transcript data MUST go through
  `hydrateSessionForTest(id, messages)` after an `adoptState` step.

## Risks

- **Delta coverage gaps.** Missing delta coverage drops message updates
  once full snapshots stop carrying transcripts. Highest-risk paths are
  message-edit operations (`set_approval_decision_on_record`,
  `set_user_input_request_state_on_record`,
  `set_mcp_elicitation_request_state_on_record`,
  `set_codex_app_request_state_on_record`) and
  `cancel_pending_interaction_messages`, which today mutate existing
  messages in place with no matching delta. Workspace-file-change
  appends via `record_active_turn_file_changes` are similarly at risk.
- **Delta-on-un-hydrated-session resync storms.** Without the explicit
  drop-and-advance-metadata policy (see Contract Precisions),
  `applyDeltaToSessions` returns `needsResync` for every delta targeting
  a summary-only session, triggering a `/api/state` refetch → another
  summary → another delta → infinite loop.
- **Remote-proxy transcript clearing.** `src/remote_sync.rs::apply_remote
  _session_to_record` rebuilds `message_positions` from
  `remote_session.messages`. If both sides speak metadata-first but the
  local proxy does not hydrate targeted sessions, every remote resync
  silently clears transcripts.
- **Transcript-bearing delta channels.** `DeltaEvent::OrchestratorsUpdated`
  and `DeltaEvent::SessionCreated` currently carry full `Session` objects;
  they preserve the transcript-size problem even after `/api/state` is
  metadata-first. Phase 1 converts them; Phase 2 exit criteria pin the
  conversion.
- **Mutation endpoints returning bare `StateResponse`.** 20+ endpoints
  today return a bare `StateResponse` that the UI feeds to `adoptState`.
  Post-cutover, the send-message response (and others) no longer carries
  the just-mutated message; UX depends on SSE delta ordering. Phase 1
  audits every endpoint and either changes the response shape or proves
  the SSE ordering.
- **UI surfaces reading `session.messages` from global state.** Enumerated
  in Phase 0's reader inventory: prompt history, search index, session
  signatures, command/diff projections, last-user-prompt lookup,
  assistant-activity checks. Each reader must be updated or explicitly
  gated on hydrated.
- **Placeholder empty-message sessions corrupting downstream state.** If
  the temporary frontend adapter leaks beyond the summary boundary,
  transcript-derived UI state collapses to empty. The discriminated
  union (Contract Precisions) prevents this at the type level but
  requires `Session.messagesLoaded: true` to be tightened first.
- **Pending-prompt attachments re-introducing the payload problem.**
  Without bounding `pending_prompts` on the summary, a user queueing
  image-attached prompts re-ships large payloads on every state
  snapshot. Contract Precisions replaces `pending_prompts` with
  `PendingPromptSummary` on the summary.
- **Test fixtures hiding incorrect assumptions.** Existing state fixtures
  include full `Session` objects by default; 169+ inline `messages: [`
  occurrences across 22 files. Phase 5's mechanical transform and the
  `assertNoTranscriptInState` guard close this.
- **Hydration response staleness across server restart.** In-flight
  `fetchSession` against instance A resolves after the UI adopts
  instance B. Contract Precisions mandates `serverInstanceId` capture at
  send and reject on mismatch at receive.
- **Mixed-version remote proxies.** Local-master talking to remote-branch
  or vice versa. Version negotiation via
  `HealthResponse.supportsMetadataFirstState` handles this; the
  compatibility branch has a hard removal deadline at Phase 5.

## Rollout Strategy

Prefer a type-safe staged rollout:

0. Capture the baseline and build the inventories (Phase 0).
1. Audit and add tests for delta coverage (Phase 1).
2. Add backend summary type and endpoint/SSE summary payloads (Phase 2).
3. Add frontend summary adoption with transcript preservation (Phase 3).
4. Remote recovery / gap semantics (Phase 4).
5. Remove temporary adapters and make transcript access impossible from
   global state types (Phase 5).

**Transitional adapter discipline.** Do not ship a long-lived "empty
messages but still typed as full Session" API. "Long-lived" is defined
concretely:

- Phase 2 and Phase 3 MUST land in the same merge series. No release tag
  cuts between them.
- The transitional `messages: []` adapter MUST NOT survive past the merge
  of Phase 3. Phase 5 immediately follows as a cleanup commit.
- CI gates: at Phase 5 merge, a schema test rejects any `StateResponse`
  that contains a `messages` field on a session, even as `[]`.
- The legacy-remote compatibility branch introduced in Phase 2 (for
  `supportsMetadataFirstState === false` remotes) is removed at the same
  deadline. Remotes that haven't updated by then fall back to the same
  "hydrate on activation" path the local UI uses, not a separate code
  path.

New UI features landing between Phase 3 and Phase 5 MUST NOT introduce new
uses of placeholder `messages: []` semantics. Reviewer discipline enforces
this; the ESLint rule added in Phase 5 codifies it.

## Completion Criteria

- `/api/state` and SSE `state` no longer include full transcripts. Schema
  test pins this at the type level.
- `DeltaEvent::SessionCreated` and `DeltaEvent::OrchestratorsUpdated` no
  longer carry full `Session` objects.
- Browser state-event parse time scales with number of sessions, not total
  conversation history. Measured against the Phase 0 baseline; delta
  recorded in `docs/prompt-responsiveness-refactor-plan.md`.
- Opening or switching to a session hydrates only that session's transcript.
  Hidden sessions do not keep transcripts resident.
- Deltas for un-hydrated sessions update summary metadata in place and do
  NOT trigger `/api/state` resyncs.
- Healthy live streaming uses deltas and does not fetch full global state
  for hydrated sessions.
- Every `StateResponse`-returning mutation endpoint either carries a
  session-bearing response where needed, or has a regression test proving
  SSE delta ordering.
- Remote proxies hydrate active panes via targeted `/api/sessions/{id}`
  fetches; remote `/api/state` is not a transcript transport.
- Version negotiation via `HealthResponse.supportsMetadataFirstState` is in
  place.
- Documentation and tests describe the metadata-first contract, including
  the hydration state machine, error semantics for `GET /api/sessions/{id}`,
  and the delta-on-un-hydrated-session policy.
- Compile-time test pins the absence of `messages` on
  `StateSessionSummary`.
- `docs/metadata-first-baseline.json` and the post-Phase-5 measurement
  both exist; the ratio confirms the improvement claim.
