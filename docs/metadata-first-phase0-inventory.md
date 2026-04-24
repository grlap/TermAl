# Metadata-First State Phase 0 Inventory

Generated: 2026-04-23.

This is the pre-implementation inventory for
`docs/metadata-first-state-plan.md`. It is intentionally conservative: any
row marked "needs audit" or "needs delta" blocks removing transcripts from
`StateResponse`.

## Baseline Status

`docs/metadata-first-baseline.json` is the **authoritative** Phase 0 baseline.
The smoke harness (`scripts/perf/prompt-responsiveness-smoke.js`) ran
successfully against a live TermAl preview at
`http://127.0.0.1:4173/?workspace=workspace-aaa32351-3b17-4224-8441-b0a43c787637`
with visible Codex tab `Codex 9772`. `smokeStatus: "captured_threshold_failures"`
in the JSON means the capture succeeded; the exit code reflects the pre-set
perf thresholds being exceeded, which is expected — the baseline documents
the problem this plan targets.

Recorded "before" numbers used as the ratchet for Phase 3 / Phase 5:

| Scenario | Metric | Baseline | Phase 5 target (plan) |
| --- | --- | --- | --- |
| active Codex typing | average next-frame delay | 28.69 ms | ≤ 10 ms |
| active Codex typing | worst next-frame delay | 384.26 ms | ≤ 30 ms |
| active Codex typing | TaskDuration (17 keystrokes) | 3.952 s | ~0.8 s ratchet |
| first key after tab switch | sinceSwitch | 239.10 ms | improved; no hard target yet |
| top frame: `handleStateEvent` | self time (typing) | 281.0 ms | summary-first path should dominate this |
| top frame: `handleStateEvent` | self time (first-key) | 459.7 ms | same |

Known baseline gaps to backfill before relying on it for Phase 5 comparison:

- The baseline does not isolate `JSON.parse` time for the SSE `state` event
  from the surrounding `handleStateEvent` work. The plan's `≤ 20 ms` parse
  threshold needs a matching "before" measurement; extend the harness to
  wrap `performance.mark` around `JSON.parse` in the SSE handler.
- The baseline does not record `/api/state` response body size in bytes.
  The plan's `< 40 KB for the 10-session fixture` target needs a baseline
  byte count against the same fixture.
- The fixture shape (session count, per-session message count, approximate
  per-message byte size) used when capturing the current numbers is not
  recorded in the JSON. Post-Phase-5 comparison needs either the same
  workspace (`workspace-aaa32351-…`) or an equivalent synthetic fixture;
  record session count / message count / total transcript byte size in the
  artifact so future runs can confirm apples-to-apples.

To rerun the live baseline, start Chrome with:

```powershell
"C:\Program Files\Google\Chrome\Application\chrome.exe" --remote-debugging-port=9222 --user-data-dir="%TEMP%\chrome-profile-stable"
```

Then open a TermAl preview page at `http://127.0.0.1:4173` with a visible
active Codex session and run:

```powershell
node scripts/perf/prompt-responsiveness-smoke.js
```

## `commit_locked` Caller Inventory

Legend:

- **Message mutation:** whether the path can add/edit/replace/delete
  `Session.messages`.
- **Delta coverage:** whether a matching narrow `DeltaEvent` already covers the
  mutation.
- **Phase 1 action:** what must happen before state snapshots can drop
  transcripts.

| Site | Scope | Message mutation | Delta coverage | Phase 1 action |
| --- | --- | --- | --- | --- |
| `src/codex_submissions.rs:337` | approval decision submission | Yes, edits approval message decision | `MessageUpdated` delta; route test covers no full-state SSE snapshot. | Record whether the HTTP `StateResponse` return shape is deferred or converted in a later phase. |
| `src/codex_submissions.rs:429` | Codex user-input submission | Yes, edits user-input request state | `MessageUpdated` delta; route test covers runtime response, no full-state SSE snapshot, and pending-map cleanup. | Record whether the HTTP `StateResponse` return shape is deferred or converted in a later phase. |
| `src/codex_submissions.rs:524` | MCP elicitation submission | Yes, edits MCP elicitation request state | `MessageUpdated` delta; route test covers runtime response, no full-state SSE snapshot, and pending-map cleanup. | Record whether the HTTP `StateResponse` return shape is deferred or converted in a later phase. |
| `src/codex_submissions.rs:611` | Codex app request reply | Yes, edits Codex app request state | `MessageUpdated` delta; route test covers runtime response, no full-state SSE snapshot, and pending-map cleanup. | Record whether the HTTP `StateResponse` return shape is deferred or converted in a later phase. |
| `src/codex_submissions.rs:633` | fail turn (e.g. after Codex app-request failure); appends file-change summary and sets SessionStatus::Error | Yes, may append file-change summary | No | Publish message-created/file-change delta. |
| `src/codex_thread_actions.rs:241` | archive Codex thread | Yes, appends markdown note | No | Return/session delta must carry note. |
| `src/codex_thread_actions.rs:301` | unarchive Codex thread | Yes, appends markdown note | No | Return/session delta must carry note. |
| `src/codex_thread_actions.rs:354` | compact Codex thread | Yes, appends markdown note | No | Return/session delta must carry note. |
| `src/codex_thread_actions.rs:442` | rollback Codex thread | Yes, may replace full transcript or append note | No | Add transcript-replaced/session-hydration invalidation path; do not rely on global state. |
| `src/orchestrator_lifecycle.rs:339` | orchestrator stop cleanup | No known transcript write | Orchestrator delta exists nearby | Verify summary-only orchestrator state remains enough. |
| `src/orchestrator_lifecycle.rs:508` | mark deadlocked orchestrators | No | Orchestrator delta path | No transcript action; keep summary metadata fresh. |
| `src/orchestrator_lifecycle.rs:546` | pending transition failure cleanup | No known transcript write | Orchestrator delta path | Verify no queued prompt transcript dependency. |
| `src/orchestrator_lifecycle.rs:566` | missing destination transition cleanup | No known transcript write | Orchestrator delta path | Verify summary metadata only. |
| `src/orchestrator_lifecycle.rs:587` | transition delivery failure cleanup | No known transcript write | Orchestrator delta path | Verify summary metadata only. |
| `src/orchestrator_lifecycle.rs:621` | transition prompt queued/delivered | No direct message write, but may queue prompt | Orchestrator delta path | Ensure pending prompt summary is bounded. |
| `src/orchestrators.rs:1056` | create orchestrator instance and backing sessions | New empty sessions only | Full state today | Convert `OrchestratorsUpdated`/create response sessions to summaries. |
| `src/remote_create_proxies.rs:84` | bind remote project id | No | Full state today | No transcript action. |
| `src/remote_create_proxies.rs:301` | create remote orchestrator proxy | May import remote sessions from remote state | Remote full state today | Requires targeted remote hydration; remote state must be summary-only. |
| `src/remote_routes.rs:371` | bind remote project id | No | Full state today | No transcript action. |
| `src/remote_routes.rs:418` | fold remote session route state | May import remote full sessions | Remote full state today | Split summary sync from targeted remote transcript hydration. |
| `src/remote_routes.rs:474` | fold remote orchestrator route state | May import remote full sessions | Remote full state today | Split summary sync from targeted remote transcript hydration. |
| `src/remote_routes.rs:504` | apply remote state snapshot | May import remote full sessions | Remote full state today | Split summary sync from targeted remote transcript hydration. |
| `src/remote_session_proxies.rs:197` | remote session kill/remove | Removes session | Full state today | Add summary removal handling; no transcript payload needed. |
| `src/session_config.rs:323` | update session settings | No | Full state today | Summary update is enough. |
| `src/session_crud.rs:399` | app settings update | No | Full state today | Summary/global metadata only. |
| `src/session_crud.rs:464` | create project | No | Full state in `CreateProjectResponse` | Summary/global metadata only. |
| `src/session_crud.rs:519` | delete project/detach sessions | No | Full state today | Summary update is enough. |
| `src/session_identity.rs:57` | set external session id | No | Full state today | Summary update is enough. |
| `src/session_identity.rs:91` | runtime-guarded external session id | No | Full state today | Summary update is enough. |
| `src/session_identity.rs:130` | clear external session id | No | Full state today | Summary update is enough. |
| `src/session_identity.rs:174` | set Codex thread state | No | Full state today | Summary update is enough. |
| `src/session_lifecycle.rs:114` | kill/remove session and hidden spares | Removes sessions | Full state today | Summary removal/create handling required. |
| `src/session_lifecycle.rs:171` | cancel queued prompt | No message write, pending prompts only | Full state today | Pending prompt summary must be bounded and updated. |
| `src/session_lifecycle.rs:397` | stop active turn | Yes, cancels pending interaction messages and appends stop/file-change messages | No | Add cancel/message-created deltas or targeted hydration invalidation. |
| `src/session_sync.rs:69` | sync Claude approval mode | No | Full state today | Summary update is enough. |
| `src/session_sync.rs:116` | sync Claude effort | No | Full state today | Summary update is enough. |
| `src/session_sync.rs:160` | sync Cursor mode | No | Full state today | Summary update is enough. |
| `src/sse_broadcast.rs:368` | late active-turn file-change summary | Yes, appends file-change message | No | Publish message-created/file-change delta. |
| `src/state_accessors.rs:297` | clear runtime state | No direct message write | Full state today | Verify pending request cleanup does not need transcript delta. |
| `src/turn_dispatch.rs:418` | dispatch queued prompt | Yes, `start_turn_on_record` adds user message | No explicit delta | Send response or delta must carry user prompt; do not rely on state. |
| `src/turn_dispatch.rs:518` | recover blocked queue with user prompt | No direct message write, queues prompt | Full state today | Pending prompt summary must be bounded. |
| `src/turn_dispatch.rs:544` | prioritize manual dispatch | Yes, starts turn and adds user message | No explicit delta | Send response or delta must carry user prompt. |
| `src/turn_dispatch.rs:568` | queue prompt while busy | No message write, queues prompt | Full state today | Pending prompt summary must be bounded. |
| `src/turn_dispatch.rs:596` | normal send dispatch | Yes, starts turn and adds user message | No explicit delta | Send response or delta must carry user prompt. |
| `src/turn_lifecycle.rs:97` | turn failed | Yes, may append failure and file-change messages | No | Add message-created/file-change delta. |
| `src/turn_lifecycle.rs:187` | assistant turn note while active | Yes, may append assistant text | No | Add message-created/text delta or targeted hydration invalidation. |
| `src/turn_lifecycle.rs:234` | turn error | Yes, may append file-change summary | No | Add message-created/file-change delta. |
| `src/turn_lifecycle.rs:302` | finish turn ok | Yes, may append file-change summary and schedule orchestrator transitions | No | Add message-created/file-change delta; keep orchestrator summary delta. |
| `src/turn_lifecycle.rs:417` | finish turn with failure/stop cleanup | Yes, cancels interaction messages and may append failure/file-change messages | No | Add cancel/message-created deltas. |
| `src/turn_lifecycle.rs:589` | cancel Claude approval request | Yes, edits approval decision messages | No | Add interaction-state/message-replaced delta. |
| `src/workspace_queries.rs:83` | update workspace layout | No | Full state today | Global metadata only. |
| `src/workspace_queries.rs:105` | delete workspace layout | No | Full state today | Global metadata only. |

Already-delta-backed paths to preserve (`commit_persisted_delta_locked`
sites, not in the table above):

- `src/session_messages.rs:80, 154, 443, 572` — push / append / replace /
  update helpers publish `MessageCreated`, `TextDelta`, `TextReplace`,
  `CommandUpdate`, and `ParallelAgentsUpdate`.
- `src/remote_routes.rs:603, 820, 927, 998` — apply remote deltas and
  republish localized deltas.
- `src/orchestrator_lifecycle.rs:64, 102, 458` — persisted delta commits
  for orchestrator status changes.
- `src/session_sync.rs:175, 208` — `note_codex_rate_limits` and
  `note_codex_notice` publish narrow `CodexUpdated` deltas.

## `StateResponse`-Returning Endpoint Inventory

These endpoints currently return a shape that the frontend can pass to
`adoptState`. Each must be assigned one of two outcomes before Phase 2:

- keep `StateResponse`, but summary-only and enough for the UX
- change to `SessionResponse` or a narrower session/message response because
  the caller needs transcript data immediately

| Frontend function | Route | Current response | Preliminary Phase 1 decision |
| --- | --- | --- | --- |
| `fetchState` | `GET /api/state` | `StateResponse` | Summary-only state. |
| `updateAppSettings` | `POST /api/settings` | `StateResponse` | Summary-only state is enough. |
| `createProject` | `POST /api/projects` | `CreateProjectResponse { state }` | Summary-only state is enough. |
| `deleteProject` | `DELETE /api/projects/{id}` | `StateResponse` | Summary-only state is enough. |
| `createOrchestratorInstance` | `POST /api/orchestrators` | `CreateOrchestratorInstanceResponse { state }` | State must be summary-only; backing sessions summary-only. |
| `pauseOrchestratorInstance` | `POST /api/orchestrators/{id}/pause` | `StateResponse` | Summary-only state is enough if orchestrator delta remains summary-only. |
| `resumeOrchestratorInstance` | `POST /api/orchestrators/{id}/resume` | `StateResponse` | Summary-only state is enough if orchestrator delta remains summary-only. |
| `stopOrchestratorInstance` | `POST /api/orchestrators/{id}/stop` | `StateResponse` | Needs audit because stop can stop sessions and append stop messages. |
| `sendMessage` | `POST /api/sessions/{id}/messages` | `StateResponse` | Change to session/message response or guarantee user-message delta before resolve. |
| `cancelQueuedPrompt` | `POST /api/sessions/{id}/queued-prompts/{promptId}/cancel` | `StateResponse` | Summary-only state with bounded pending prompts should be enough. |
| `submitApproval` | `POST /api/sessions/{id}/approvals/{messageId}` | `StateResponse` | Updated interaction message also arrives via `MessageUpdated`; response-shape conversion is deferred. |
| `submitUserInput` | `POST /api/sessions/{id}/user-input/{messageId}` | `StateResponse` | Updated interaction message also arrives via `MessageUpdated`; response-shape conversion is deferred. |
| `submitMcpElicitation` | `POST /api/sessions/{id}/mcp-elicitation/{messageId}` | `StateResponse` | Updated interaction message also arrives via `MessageUpdated`; response-shape conversion is deferred. |
| `submitCodexAppRequest` | `POST /api/sessions/{id}/codex/requests/{messageId}` | `StateResponse` | Updated interaction message also arrives via `MessageUpdated`; response-shape conversion is deferred. |
| `updateSessionSettings` | `POST /api/sessions/{id}/settings` | `StateResponse` | Summary-only state is enough for metadata fields. |
| `renameSession` | `POST /api/sessions/{id}/settings` | `StateResponse` | Summary-only state is enough. |
| `refreshSessionModelOptions` | `POST /api/sessions/{id}/model-options/refresh` | `StateResponse` | Summary-only state is enough. |
| `forkCodexThread` | `POST /api/sessions/{id}/codex/thread/fork` | `CreateSessionResponse` | Already session-bearing; keep. |
| `archiveCodexThread` | `POST /api/sessions/{id}/codex/thread/archive` | `StateResponse` | Appends note; change response or add delta. |
| `unarchiveCodexThread` | `POST /api/sessions/{id}/codex/thread/unarchive` | `StateResponse` | Appends note; change response or add delta. |
| `compactCodexThread` | `POST /api/sessions/{id}/codex/thread/compact` | `StateResponse` | Appends note; change response or add delta. |
| `rollbackCodexThread` | `POST /api/sessions/{id}/codex/thread/rollback` | `StateResponse` | Replaces transcript; change response or force targeted hydration. |
| `killSession` | `POST /api/sessions/{id}/kill` | `StateResponse` | Summary removal is enough. |
| `stopSession` | `POST /api/sessions/{id}/stop` | `StateResponse` | Appends/cancels messages; change response or add deltas. |

Remote proxy methods returning `StateResponse` each inherit the decision of
their non-remote counterpart in the main table above. The same audit and
delta/response conversion that apply locally apply to the remote proxy call:

| Remote proxy method | Non-remote counterpart | Inherited decision |
| --- | --- | --- |
| `proxy_remote_stop_session` | `stopSession` | Appends/cancels messages — change response or add deltas. |
| `proxy_remote_kill_session` | `killSession` | Summary removal is enough. |
| `proxy_remote_archive_codex_thread` | `archiveCodexThread` | Appends note — change response or add delta. |
| `proxy_remote_unarchive_codex_thread` | `unarchiveCodexThread` | Appends note — change response or add delta. |
| `proxy_remote_compact_codex_thread` | `compactCodexThread` | Appends note — change response or add delta. |
| `proxy_remote_rollback_codex_thread` | `rollbackCodexThread` | Replaces transcript — change response or force targeted hydration. |
| `proxy_remote_pause_orchestrator_instance` | `pauseOrchestratorInstance` | Summary-only state is enough. |
| `proxy_remote_resume_orchestrator_instance` | `resumeOrchestratorInstance` | Summary-only state is enough. |
| `proxy_remote_stop_orchestrator_instance` | `stopOrchestratorInstance` | Needs audit (may cascade to session stops that append messages). |

Remote state-sync entry points:

- `apply_remote_state_snapshot` — consumes remote summaries for global sync;
  transcript repair must go through targeted remote `GET /api/sessions/{id}`.
- `sync_remote_state_for_target` — same contract as `apply_remote_state_snapshot`.

These must NOT rely on remote `/api/state` carrying transcripts after the
refactor; the transition is the same as the local-UI hydration path.

## Frontend `session.messages` Reader Inventory

Production readers outside the transcript card renderer:

| File | Readers | Phase 3 action |
| --- | --- | --- |
| `ui/src/app-utils.ts` | conversation signature, candidate source paths, last user prompt | Gate on hydrated session or move derived values into hydrated store. |
| `ui/src/live-updates.ts` | delta reducer reads and mutates message arrays | Add explicit unhydrated-session delta policy: update summary metadata, drop transcript payload, do not resync. |
| `ui/src/session-find.ts` | search index flattens messages | Disable/search-loading for summary-only sessions until hydrated. |
| `ui/src/session-store.ts` | prompt history derives from messages | Preserve prompt history across summary snapshots; recompute only for hydrated sessions. |
| `ui/src/session-reconcile.ts` | `reconcileSession` at line 98 reads `previous.messages` and `next.messages`; line 101 compares message-array identity via `sameSessionSummary` fast-path | New contract per plan Phase 3: when `next.messagesLoaded === false`, preserve `previous.messages` verbatim and merge non-transcript fields from `next`; existing deep-reconcile path runs only when both sides are hydrated. |
| `ui/src/app-live-state.ts` | line 957: `allowRevisionDowngrade: currentSession.messages.length === 0` — staleness gate in `adoptFetchedSession` | Replace `messages.length === 0` with `messagesLoaded !== true` per plan Phase 3. |
| `ui/src/SessionPaneView.tsx` | command/diff projections and last-message author checks | Require hydrated session or show loading/disabled projections. |
| `ui/src/panels/AgentSessionPanel.tsx` | passes messages into virtual list and composer state | Active pane triggers hydration; composer behavior must be chosen and tested. |
| `ui/src/panels/VirtualizedConversationMessageList.tsx` | transcript rendering and measurement | Hydrated-only input. |
| `ui/src/panels/session-message-leaves.tsx` | prompt-history scan | Hydrated-only helper or store-derived prompt history. |
| `ui/src/control-surface-state.ts` | `mergeOrchestratorDeltaSessions` currently accepts `Session[]` input (typing, not direct `.messages` read) | Change to accept `StateSessionSummary[]` and preserve hydrated records. |

Hydration scaffolding already exists in:

- `ui/src/app-live-state.ts:927` `adoptFetchedSession`
- `ui/src/app-live-state.ts:980` effect for `messagesLoaded === false`

Both sites are covered by the `ui/src/app-live-state.ts` row above; the
staleness-check update at line 957 is the primary code change that Phase 3
owes.

## Fixture Inventory

`rg -l "messages:\\s*\\[" ui/src src/tests` currently matches 23 files:

- `src/tests/persist.rs`
- `ui/src/App.live-state.deltas.test.tsx`
- `ui/src/App.live-state.reconnect.test.tsx`
- `ui/src/App.live-state.visibility.test.tsx`
- `ui/src/App.live-state.watchdog.test.tsx`
- `ui/src/App.orchestrators.test.tsx`
- `ui/src/App.scroll-behavior.test.tsx`
- `ui/src/App.session-lifecycle.test.tsx`
- `ui/src/App.workspace-files-changed.test.tsx`
- `ui/src/app-test-harness.tsx`
- `ui/src/backend-connection.test.tsx`
- `ui/src/live-updates.test.ts`
- `ui/src/mockData.ts`
- `ui/src/panels/AgentSessionPanel.test.tsx`
- `ui/src/panels/OrchestratorTemplatesPanel.test.tsx`
- `ui/src/panels/PaneTabs.test.tsx`
- `ui/src/panels/SessionCanvasPanel.test.tsx`
- `ui/src/session-find.test.ts`
- `ui/src/session-list-filter.test.ts`
- `ui/src/session-model-refresh.test.tsx`
- `ui/src/session-reconcile.test.ts`
- `ui/src/session-store.test.ts`
- `ui/src/workspace.test.ts`

Phase 5 should add `assertNoTranscriptInState(state)` and convert state
fixtures to summary-first helpers. Tests that need transcripts should hydrate
explicitly after adopting summary state.

## Reviewer Checkpoint

This is a suitable first review checkpoint before behavior changes. The
expected reviewer focus:

- confirm the `commit_locked` message-mutating rows are complete
- confirm endpoint decisions are acceptable
- confirm no critical frontend `session.messages` reader is missing
- confirm the authoritative baseline numbers and the listed baseline gaps are
  acceptable for the Phase 3 / Phase 5 comparison
