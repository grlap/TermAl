# Rust Type-Safety Hardening Plan

TermAl already benefits from Rust's type system, but several backend invariants
are still enforced by discipline and convention. This plan moves the riskiest
conventions into types, helper APIs, and tests so the safe path becomes the
normal path.

This is not a broad rewrite. The target is the small set of places where a
missed convention can corrupt persistence, lose UI state, publish invalid SSE
events, or mix local and remote identities.

## Design Principle

Prefer APIs where the safe path is the only normal path.

When code currently depends on comments such as "remember to call this helper"
or "do not mutate this field directly", introduce a type boundary that encodes
the rule. Comments should explain why the boundary exists; the compiler should
enforce as much of the boundary as practical.

## Current Status

Node 0 is implemented in the current tree:

- `src/remote_sync.rs` now has a named `RemoteSyncRollback` snapshot.
- Rollback restores `removed_session_ids` alongside sessions,
  orchestrator instances, and `next_session_number`.
- `src/tests/remote.rs::failed_remote_snapshot_sync_restores_session_tombstones`
  covers the stale tombstone failure mode.
- `docs/bugs.md` moved the stale tombstone bug into the fixed preamble.

The rest of the plan is still pending.

## Implementation Nodes

### Node 0: Remote-Sync Rollback Snapshot

**Status:** Done.

**Problem:** `sync_remote_state_inner` could queue tombstones through
`retain_sessions`, fail later during orchestrator localization, restore the
session list, and leave stale `removed_session_ids` queued for SQLite delete.

**Design:** Use a named rollback type instead of an ad hoc tuple.

```rust
struct RemoteSyncRollback {
    next_session_number: usize,
    sessions: Vec<SessionRecord>,
    orchestrator_instances: Vec<OrchestratorInstance>,
    removed_session_ids: Vec<String>,
}
```

**Implementation notes:**

- Keep rollback capture local to remote sync for now.
- Snapshot `removed_session_ids` whenever snapshotting `sessions`.
- Use the same rollback helper in `ensure_remote_orchestrator_instance`, since
  it can also create proxy sessions while localizing an orchestrator.

**Coverage:**

- Force a remote snapshot to omit a referenced remote session.
- Let broad sync queue a tombstone for the missing proxy session.
- Force orchestrator localization to fail because the omitted session is still
  referenced.
- Assert rollback restores the proxy session and leaves no stale tombstone.

### Node 1: Tighten `CreateSessionResponse`

**Status:** Done.

**Problem:** `CreateSessionResponse` currently allows invalid shapes through
optional fields. The frontend must handle `{ sessionId }` or other incomplete
responses even though current backend producers always emit a concrete session
plus revision.

**Decision:** Do not introduce a two-variant enum yet. No production path emits
the state-shaped response today. Prefer the simpler current contract:
`sessionId`, `session`, and `revision` are required.

**Preferred design:**

```rust
struct CreateSessionResponse {
    session_id: String,
    session: Session,
    revision: u64,
}
```

If a future route genuinely needs to return a full `StateResponse`, introduce an
explicit enum at that time:

```rust
#[serde(untagged)]
enum CreateSessionResponse {
    Session { session_id: String, session: Session, revision: u64 },
    State { session_id: String, state: StateResponse },
}
```

**Implementation notes:**

- Updated the Rust DTO in `src/wire.rs`.
- Updated all current construction sites:
  - `src/session_crud.rs`
  - `src/codex_thread_actions.rs`
  - `src/remote_create_proxies.rs`
  - `src/remote_codex_proxies.rs`
- Updated the frontend API type and adoption path so it no longer treats
  `session` as optional for this response.
- Kept the compatibility surface out of the Rust wire contract; no current
  production path emits the old state-shaped response.
- Keep any compatibility adapter local to the frontend only if older dev builds
  still need to be tolerated during manual testing. Do not keep the Rust wire
  contract optional just for that.

**Coverage:**

- Rust route coverage proves create-session JSON includes `sessionId`, `session`, and
  `revision`.
- App-level frontend coverage now resolves create-session flows with the
  required `{ sessionId, session, revision }` shape.

**Completion criteria:**

- No backend construction site can compile without `session` and `revision`.
- The TypeScript response type marks `session` and `revision` required.
- `docs/bugs.md` entry for the weak response contract moved to fixed.

### Node 2a: Fix `session_mut_by_index` Stamp Leak

**Status:** Pending.

**Problem:** `session_mut_by_index` advances `last_mutation_stamp` before it
checks whether the index exists. An out-of-bounds miss burns a stamp without
stamping a record.

**Preferred design:**

```rust
fn session_mut_by_index(&mut self, index: usize) -> Option<&mut SessionRecord> {
    if index >= self.sessions.len() {
        return None;
    }
    let stamp = self.next_mutation_stamp();
    let record = self.sessions.get_mut(index)?;
    record.mutation_stamp = stamp;
    Some(record)
}
```

The exact implementation can avoid the explicit bounds check if it satisfies
the same invariant: no stamp advances unless a record exists.

**Implementation notes:**

- Keep this as a narrow bug fix before introducing a wrapper.
- Check `session_mut` and `session_mut_by_index` miss behavior stays aligned.
- Do not change no-op mutation behavior in this node.

**Coverage:**

- Extend the existing session mutation helper test with an out-of-bounds call.
- Assert `last_mutation_stamp` does not advance on miss.
- Assert valid mutation still stamps the record.

**Completion criteria:**

- The active `docs/bugs.md` stamp-leak entry can be moved to fixed.

### Node 2b: Introduce Stamped Session Mutation Wrappers

**Status:** Pending.

**Problem:** Persistence correctness depends on production code mutating
sessions through stamp-aware helpers. Raw `inner.sessions[index]` mutation can
skip `mutation_stamp`, causing SQLite delta persistence to miss changes.

**Preferred design:**

```rust
struct StampedSessionMut<'a> {
    record: &'a mut SessionRecord,
}
```

`StateInner` should expose session mutation through helpers that return this
wrapper rather than a plain `&mut SessionRecord`.

**Implementation notes:**

- The crate currently uses `include!()` fragments, so module privacy cannot
  fully hide `StateInner.sessions`. The wrapper still helps because call-site
  types and helper names make the stamping requirement explicit.
- Start with high-risk production paths:
  - remote sync
  - project deletion
  - session creation/fork whole-record replacement
  - orchestrator queued-prompt cleanup
  - turn lifecycle paths that update messages or runtime state
- Decide whether the wrapper implements `DerefMut<Target = SessionRecord>` or
  exposes narrower methods. Start pragmatic with `DerefMut`, then narrow later
  only where it removes real risk.
- Add a conditional helper for no-op updates only if write amplification remains
  measurable:

```rust
fn mutate_session_if_changed<F>(&mut self, index: usize, mutate: F) -> Option<bool>
where
    F: FnOnce(&mut SessionRecord) -> bool;
```

**Coverage:**

- Tests for wrapper construction stamping the record.
- Tests for no stamp on missing index.
- Regression coverage for at least one production path that previously needed
  a manual re-stamp after whole-record replacement.

**Completion criteria:**

- New production session mutations prefer wrapper-returning helpers.
- Remaining direct `inner.sessions` mutable access sites are audited and either
  converted or documented as read-only/index-only patterns.

### Node 3: Add Local/Remote ID Newtypes in Remote Sync

**Status:** Pending.

**Problem:** Remote sync handles local IDs, remote IDs, project IDs, session IDs,
orchestrator IDs, and remote host IDs in the same functions. Plain `String`
values make accidental identity-space mixups easy.

**Scope:** Start inside remote-sync internals only. Do not convert every wire DTO
or frontend-facing type in the first pass.

**Preferred design:**

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LocalSessionId(String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RemoteSessionId(String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LocalProjectId(String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RemoteProjectId(String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LocalOrchestratorId(String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RemoteOrchestratorId(String);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RemoteId(String);
```

**Borrowing ergonomics:** Define the borrowing shape before converting call
sites.

```rust
impl RemoteSessionId {
    fn as_str(&self) -> &str { &self.0 }
}

impl AsRef<str> for RemoteSessionId {
    fn as_ref(&self) -> &str { self.as_str() }
}

impl std::fmt::Display for RemoteSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
```

Avoid forcing every caller into `.0.as_str()`.

**Implementation notes:**

- Convert maps first:
  - `HashMap<RemoteProjectId, LocalProjectId>`
  - remote session lookup maps where possible
- Keep conversion to/from wire strings at the boundary.
- Prefer helper signatures that communicate identity space:

```rust
fn local_session_id_for_remote_session(
    remote_id: &RemoteId,
    remote_session_id: &RemoteSessionId,
) -> Option<LocalSessionId>
```

**Coverage:**

- Existing remote sync tests should continue to pass.
- Add at least one compile-time-only style assertion through function
  signatures: a helper should not accept a plain `String` where a remote ID is
  required.

**Completion criteria:**

- The densest remote-to-local mapping helpers no longer accept plain `&str` for
  both local and remote identity spaces.

### Node 4: Clarify ACP Runtime State

**Status:** Pending.

**Problem:** ACP readiness is represented by a loose mix of options and flags in
shared runtime state. The actual flow is stateful: initialize, authenticate,
load or create a session, refresh config, then prompt.

**Decision:** Do not start with typestate generics. ACP state is shared through
`Arc<Mutex<_>>` across reader/writer/request paths, so typestate would mostly
disappear behind the lock. Use explicit state structs and enums instead.

**Preferred design:**

```rust
struct AcpInitializedCapabilities {
    supports_session_load: bool,
}

struct AcpActiveSession {
    session_id: String,
    is_loading_history: bool,
}

struct AcpRuntimeState {
    capabilities: Option<AcpInitializedCapabilities>,
    active_session: Option<AcpActiveSession>,
}
```

An enum may be better if the transitions become clearer:

```rust
enum AcpSessionState {
    NotStarted,
    Loading { requested_session_id: Option<String> },
    Active(AcpActiveSession),
}
```

**Implementation notes:**

- Replace `current_session_id: Option<String>` with a typed active-session
  field.
- Replace `supports_session_load: Option<bool>` with initialized capabilities.
- Keep `is_loading_history` attached to the active/loading session state rather
  than as an unrelated boolean.
- Localize Gemini/Cursor session-load fallback behavior near the transition that
  handles the failed load.

**Coverage:**

- Existing ACP load/new fallback tests.
- New test proving prompt dispatch cannot proceed without an active ACP session.
- New test proving capabilities are populated by initialize before config
  refresh uses them.

**Completion criteria:**

- Prompt/config helpers read active session state from a typed field, not a
  bare `Option<String>`.
- ACP session-load fallback is represented as a state transition, not scattered
  `Option` checks.

### Node 5: Two-Layer Orchestrator Lifecycle

**Status:** Pending.

**Problem:** Orchestrator lifecycle behavior is spread across `AppState` methods,
status checks, mutable fields, and side-effect helpers. A pure
`OrchestratorInstance::pause()` method is not enough because real transitions
also need access to other sessions, SSE fan-out, and stopped-session tracking.

**Decision:** Use a two-layer design:

- `AppState` remains the coordinator for cross-state side effects.
- `OrchestratorInstance` or a lifecycle enum owns pure status transition rules.

**Preferred shape:**

```rust
enum OrchestratorLifecycle {
    Running(RunningInstance),
    Paused(PausedInstance),
    Stopping(StoppingInstance),
    Stopped(StoppedInstance),
}
```

The coordinator still performs side effects:

```rust
impl AppState {
    fn pause_orchestrator_instance(&self, id: &str) -> Result<StateResponse, ApiError> {
        // lock state
        // ask lifecycle object for transition
        // mutate related sessions
        // publish orchestrator update
        // commit
    }
}
```

**Implementation notes:**

- First extract pure transition helpers that validate status changes and return
  a typed outcome.
- Keep `AppState` methods as the public API.
- Move side effects into one coordinator path per transition:
  - pause
  - resume
  - begin stop
  - finish stop
  - force stop
- Avoid a split where callers must remember to call both "pure transition" and
  "side effects" manually.

**Coverage:**

- Existing orchestrator lifecycle tests.
- Tests for invalid transitions returning conflicts.
- Tests that stop still updates related sessions and publishes the expected SSE
  state.

**Completion criteria:**

- Lifecycle status changes have one pure implementation point.
- Public `AppState` lifecycle methods remain the only place that performs
  cross-session side effects.

### Node 6: Commit-Variant Typing

**Status:** Deferred until Nodes 2a and 2b are done.

**Problem:** `commit_locked`, `commit_delta_locked`, and
`commit_session_created_locked` are a compile-time-silent three-way choice. The
wrong commit variant can publish too much, publish too little, or skip durable
persistence expectations.

**Decision:** Prefer commit-variant typing over standalone `LocalRevision` /
`RemoteRevision` newtypes. Revision mixups are not the concrete bug. The more
useful boundary is coupling a mutation scope to the commit/publish operation
that completes it.

**Possible design:**

```rust
struct FullSnapshotMutation;
struct DeltaMutation {
    event: DeltaEvent,
}
struct SessionCreatedMutation {
    record: SessionRecord,
}
```

Or make the stamped session wrapper carry the required commit scope:

```rust
struct StampedSessionMut<'a, Scope> {
    record: &'a mut SessionRecord,
    scope: std::marker::PhantomData<Scope>,
}
```

**Implementation notes:**

- Do not start here. This depends on the mutation wrapper work.
- Start with one high-value path, likely delta message updates.
- The helper that bumps the revision should return the revision used in the
  matching `DeltaEvent`.

**Coverage:**

- Tests that delta-only mutations publish deltas and do not publish full
  snapshots.
- Tests that session-created mutations persist the created row and publish the
  expected creation event.

**Completion criteria:**

- At least one mutation family cannot compile unless completed through the
  correct commit helper.

## Deferred Items

### Revision Newtypes

Standalone `LocalRevision` and `RemoteRevision` newtypes are deferred. Current
code usually keeps local and remote revisions distinct by naming, and the main
revision invariant is enforced at the commit helpers. Revisit this only if a
concrete miswiring bug appears.

### Unified Pending-Request Register

`SessionRecord` carries several parallel in-flight request maps:

- `pending_claude_approvals`
- `pending_codex_approvals`
- `pending_codex_user_inputs`
- `pending_codex_mcp_elicitations`
- `pending_codex_app_requests`
- `pending_acp_approvals`

A shared trait or tagged enum for in-flight interaction requests could unify
registration, expiration, cancellation-on-runtime-exit, and preview rendering.
This is useful but not next up because it may trade away per-agent typed payload
clarity.

### RuntimeToken Guard Wrapper

The `_if_runtime_matches` pattern is manually applied around runtime mutations.
A `GuardedRuntime<'a, T>` type could hand out mutable access only after the
runtime token is proven current. This is a good future hardening target after
the session mutation wrapper is in place.

## Recommended Order

1. Node 0: remote-sync rollback snapshot. Done.
2. Node 1: tighten `CreateSessionResponse` to required `session + revision`.
   Done.
3. Node 2a: fix `session_mut_by_index` miss semantics.
4. Node 2b: introduce stamped session mutation wrappers.
5. Node 3: add local/remote ID newtypes inside remote-sync internals.
6. Node 4: clarify ACP runtime state with explicit structs/enums.
7. Node 5: rework orchestrator lifecycle into coordinator plus typed transition
   rules.
8. Node 6: explore commit-variant typing once mutation wrappers exist.

Each node should land with focused regression coverage and a `docs/bugs.md`
update when it closes an active bug. The value comes from shrinking the number
of states reviewers must hold in their heads, not from adding abstraction for
its own sake.
