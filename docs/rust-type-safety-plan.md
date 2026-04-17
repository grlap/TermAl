# Rust Type-Safety Hardening Plan

TermAl already benefits from Rust's type system, but several backend invariants
are still enforced by discipline and convention. The goal of this plan is to
move the most failure-prone conventions into types and APIs that make invalid
states difficult or impossible to express.

This is not a broad rewrite plan. The target is the small set of places where a
missed convention can corrupt persistence, lose UI state, publish invalid SSE
events, or mix local and remote identities.

## Design Principle

Prefer APIs where the safe path is the only normal path.

When code currently depends on comments such as "remember to call this helper"
or "do not mutate this field directly", introduce a type boundary that encodes
the rule. Comments should explain why the boundary exists; the compiler should
enforce as much of the boundary as practical.

## Priority 1: Enforce Create-Session Response Shape

Current risk: `CreateSessionResponse` uses multiple optional fields, so the
type permits invalid responses where neither `session` nor `state` is present.
The frontend adapter must defensively handle a shape the backend should never
emit.

Preferred design:

```rust
#[serde(untagged)]
enum CreateSessionResponse {
    Session {
        session_id: String,
        session: Session,
        revision: LocalRevision,
    },
    State {
        session_id: String,
        state: StateResponse,
    },
}
```

Expected outcome:

- Every serialized create-session response contains enough data for the frontend
  to adopt the new session.
- A `{ sessionId }`-only response cannot be constructed accidentally.
- Tests can assert both valid variants and reject incomplete shapes.

> **Review note — premise accurate, design possibly over-generalized.**
> Every production construction site emits `session: Some(…), revision, state: None`:
>
> - `src/session_crud.rs:306`
> - `src/codex_thread_actions.rs:176`
> - `src/remote_create_proxies.rs:196`
> - `src/remote_codex_proxies.rs:124`
>
> No path produces the `State` variant today. The two-variant enum futures-proofs
> a shape nobody emits. Simpler alternative: drop the `state` field and make
> `session` + `revision` required. If a `State`-variant caller is actually
> planned, keep the enum; if not, the enum adds serde surface for no behavioral
> gain.

## Priority 2: Type Remote-Sync Rollback State

Current risk: remote sync rollback depends on manually snapshotting every field
that may be mutated before a fallible step. If a new rollback-sensitive field is
added and not included in the tuple, the rollback becomes partial.

Preferred design:

```rust
struct RemoteSyncRollback {
    next_session_number: usize,
    sessions: Vec<SessionRecord>,
    orchestrator_instances: Vec<OrchestratorInstance>,
    removed_session_ids: Vec<String>,
}
```

Possible stronger design:

```rust
struct RemoteSyncTransaction<'a> {
    inner: &'a mut StateInner,
    rollback: RemoteSyncRollback,
    committed: bool,
}
```

Expected outcome:

- Rollback contents are named and reviewable.
- Tombstone handling is part of the rollback contract.
- Future remote-sync mutations have an obvious place to declare rollback
  requirements.

> **Review note — this catches a latent SQLite-correctness bug; consider
> promoting to Priority 1.**
>
> Current rollback at `src/remote_sync.rs:85-91` captures a 3-tuple:
>
> ```rust
> let pre_orchestrator_rollback_state = focus_remote_session_id.is_none().then(|| {
>     (inner.next_session_number, inner.sessions.clone(), inner.orchestrator_instances.clone())
> });
> ```
>
> Line 109 calls `inner.retain_sessions(|record| …)` which invokes
> `record_removed_session` on each dropped session and appends to
> `removed_session_ids`. If a later step in `sync_remote_state_inner` fails and
> triggers rollback (lines 185-188), `sessions` and
> `orchestrator_instances` are restored from the tuple but `removed_session_ids`
> is not — it stays queued. The next persist tick then issues
> `DELETE WHERE id = ?` for sessions that have just been rolled back into
> existence. The plan's named `RemoteSyncRollback` struct with an explicit
> `removed_session_ids` field would make this impossible to miss.
>
> The stronger `RemoteSyncTransaction<'a>` design with a `committed: bool` flag
> and `Drop`-based rollback is worth pursuing in the same work — it turns
> "forgot to commit" into a pattern the reviewer can see from the control-flow
> shape.

## Priority 3: Make Session Mutation Stamping Structural

Current risk: persistence correctness depends on mutating sessions through
stamp-aware helpers such as `session_mut_by_index`. Raw mutation paths can skip
the `mutation_stamp`, causing SQLite delta persistence to miss changes.

Preferred design:

```rust
struct StampedSessionMut<'a> {
    record: &'a mut SessionRecord,
}
```

Expose session mutation through `StateInner` methods that return a stamped
wrapper instead of a raw `&mut SessionRecord`. Keep the sessions vector private
to the mutation API as much as the current flat-module layout permits.

Expected outcome:

- Production session mutation goes through a type that has already stamped the
  record.
- Direct `inner.sessions[index]` mutation becomes rare, visible, and easy to
  audit.
- No-op mutations can be handled deliberately, either by accepting the extra
  stamp or by introducing a conditional-mutation helper.

> **Review note — overlaps with an active bug; land the narrow fix first.**
>
> `docs/bugs.md` still tracks "`session_mut_by_index` leaks a mutation stamp on
> out-of-bounds miss": `next_mutation_stamp()` runs before `self.sessions.get_mut(index)`
> can fail, so an OOB call burns a stamp without stamping anything. That bug has
> a four-line fix (reorder to `get_mut` first, advance stamp inside the `Some`
> arm) that can ship independently of the full `StampedSessionMut<'a>` wrapper.
> Do the small fix now so the bug closes; the wrapper in this priority
> subsumes it later.
>
> Scope note: the crate currently compiles through `include!()`, so "sessions
> vector private to the mutation API" is enforced only by convention — there is
> no `mod` boundary to put `pub(super)` behind. The wrapper is still worth it:
> type errors at call sites are useful even without module-level visibility
> enforcement. Worth acknowledging explicitly in the plan so the enforcement
> ceiling is clear.

## Priority 4: Introduce Local and Remote ID Newtypes

Current risk: remote sync frequently handles local IDs, remote IDs, project IDs,
session IDs, orchestrator IDs, and remote host IDs in the same scope. Plain
`String` values make accidental mixups easy.

Preferred design:

```rust
struct LocalSessionId(String);
struct RemoteSessionId(String);
struct LocalProjectId(String);
struct RemoteProjectId(String);
struct LocalOrchestratorId(String);
struct RemoteOrchestratorId(String);
struct RemoteId(String);
```

Start in remote-sync code rather than converting the whole backend at once.
Conversion helpers can keep serde and UI-facing DTOs string-shaped while the
internal mapping logic gets stronger types.

Expected outcome:

- Remote-to-local mapping code becomes harder to misuse.
- Function signatures communicate which identity space they expect.
- Reviewers can spot accidental boundary crossing from type errors instead of
  manually tracing string provenance.

> **Review note — scope is right; flag the ergonomics cost.**
>
> Starting in remote-sync internals is the correct scope. One thing worth
> flagging up front: `struct RemoteSessionId(String)` changes every `.as_deref()`
> call site into either a `Deref<Target = str>` impl, an `as_str()` method, or
> an explicit `.0.as_str()` chain. Code that currently reads
> `record.remote_session_id.as_deref()` will want `record.remote_session_id.as_deref().map(RemoteSessionId::as_str)`
> or similar. Not a blocker, but worth planning the borrowing shape
> (`AsRef<str>` + `Deref` + `Display`) before the first conversion so the call
> sites don't churn twice.

## Priority 5: Centralize Revision Construction

Current risk: outbound deltas can be constructed with raw revision values,
including stale or duplicate revisions. The monotonic revision contract is
documented, but direct `u64` use weakens enforcement.

Preferred design:

```rust
struct LocalRevision(u64);
struct RemoteRevision(u64);
```

Avoid public constructors that accept arbitrary values in mutation paths. Prefer
commit/publish helpers that bump the revision and return the value used by the
corresponding SSE event.

Expected outcome:

- Local and remote revisions cannot be accidentally interchanged.
- Delta events are more tightly coupled to the commit that produced them.
- Duplicate-revision publishes become easier to prevent and test.

> **Review note — weakest motivation of the set; consider deferring.**
>
> Unlike the other priorities, this one does not name a specific bug that the
> newtype would have prevented. In practice `sync_remote_state_inner` already
> keeps `remote_state.revision` and `inner.revision` clearly distinct by name,
> and the monotonic revision contract is guarded at its source (`bump_revision_and_persist_locked`
> in `sse_broadcast.rs`) rather than at every delta-event construction site.
> The "Delta events are more tightly coupled to the commit that produced them"
> outcome is probably better pursued through Priority 3's commit-variant typing
> (see the "Candidates for future additions" section below) than through a
> separate revision-newtype effort.
>
> Suggested: defer this until a concrete mis-wiring bug surfaces, or fold the
> delta-commit coupling piece into Priority 3.

## Priority 6: Encode Orchestrator Lifecycle Transitions

Current risk: orchestrator lifecycle behavior is spread across status checks and
mutable fields. It is possible to update status without updating related
instance/session bookkeeping.

Pragmatic first step:

```rust
impl OrchestratorInstance {
    fn pause(&mut self) -> Result<()>;
    fn resume(&mut self) -> Result<()>;
    fn mark_stopping(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}
```

Possible stronger design:

```rust
enum OrchestratorLifecycle {
    Running(RunningInstance),
    Paused(PausedInstance),
    Stopping(StoppingInstance),
    Stopped(StoppedInstance),
}
```

Expected outcome:

- Status transitions have one implementation point.
- Side effects such as queued prompt cleanup and transition blocking are easier
  to co-locate with the transition that requires them.
- Invalid transitions can return typed conflicts instead of relying on scattered
  guard logic.

> **Review note — the proposed `self` receiver needs clarifying.**
>
> The existing transitions in `src/orchestrator_lifecycle.rs` —
> `pause_orchestrator_instance`, `resume_orchestrator_instance`,
> `begin_orchestrator_stop`, `finish_orchestrator_stop`,
> `stop_orchestrator_instance` — hang off `AppState`, not
> `OrchestratorInstance`. They need access to:
>
> - `self.inner` to find and mutate *other* sessions (the stop cascade kills
>   child sessions across the whole session list).
> - `self.publish_orchestrators_updated` for SSE fan-out.
> - `self.note_stopped_orchestrator_session` for cross-instance stopping-state
>   tracking.
>
> `&mut OrchestratorInstance` cannot reach any of that. If the plan lands as
> written, it would either (a) leave the coordination methods on `AppState`
> with `OrchestratorInstance::pause` as an internal helper that only flips the
> status enum, or (b) split every transition into "pure lifecycle state change"
> (instance) and "orchestration side effects" (AppState) methods that must be
> called together. Option (a) is a smaller win than the plan suggests. Option
> (b) is the larger typestate-enum design and is a real improvement — but worth
> calling out as the actual reshape, not a "pragmatic first step".
>
> The typestate `enum OrchestratorLifecycle { Running(…), Paused(…), Stopping(…), Stopped(…) }`
> still needs an `AppState`-level coordinator that matches on the enum and
> performs the side effects. Document the two-layer shape so reviewers know
> what they are committing to.

## Priority 7: Clarify ACP Runtime State

Current risk: ACP runtime readiness is represented through a mix of options,
capabilities, request maps, and runtime state fields. The real protocol flow is
stateful: spawn, initialize, authenticate, load or create a session, refresh
config, then prompt.

Pragmatic first step:

- Add explicit state structs for initialized capabilities, active ACP session
  identity, and pending request ownership.
- Keep one runtime implementation, but reduce loose `Option` fields where a
  state transition can prove the value exists.

Possible stronger design:

```rust
struct AcpRuntime<Initializing> { /* ... */ }
struct AcpRuntime<Ready> { /* ... */ }
```

Expected outcome:

- Prompt dispatch can require a ready ACP session instead of checking readiness
  by convention.
- Session load fallback behavior becomes local to the transition that handles
  load failure.
- Pending JSON-RPC request ownership and timeout behavior are easier to audit.

> **Review note — endorse the pragmatic framing; explain why typestate is
> awkward here.**
>
> Current shape at `src/runtime.rs:258`:
>
> ```rust
> struct AcpRuntimeState {
>     current_session_id: Option<String>,
>     is_loading_history: bool,
>     supports_session_load: Option<bool>,
> }
> ```
>
> The 5-phase handshake documented in `src/acp.rs`'s file-level block
> (initialize → authenticate → session/load-or-new → set_mode → set_model →
> prompt) is not encoded anywhere; every helper (`handle_acp_prompt_command`,
> `configure_acp_session`, the six callers of `&Arc<Mutex<AcpRuntimeState>>`)
> defensively peeks at `current_session_id.as_ref()`. Typed state structs
> (`AcpInitializedCapabilities`, `AcpActiveSession`) that replace the loose
> `Option` fields would land cleanly.
>
> The typestate version (`AcpRuntime<Initializing>` → `AcpRuntime<Ready>`) is
> awkward specifically because the runtime is shared as `Arc<Mutex<…>>` across
> a writer thread, a reader thread, and the main request path. Typestate
> through a Mutex loses most of its value: the type parameter disappears behind
> the lock, and the state transitions move from the type system back into
> runtime checks. The plan is right to stop at "pragmatic first step"; worth
> naming the Mutex-typestate incompatibility so future readers don't try the
> typestate version first.

## Implementation Order

1. Convert `CreateSessionResponse` to an enum and update frontend adapters/tests.
2. Add `RemoteSyncRollback` and use it in `sync_remote_state_inner`.
3. Introduce stamped session mutation wrappers for the highest-risk mutation
   paths.
4. Add local/remote ID newtypes inside remote-sync internals.
5. Introduce `LocalRevision` and `RemoteRevision` where deltas are published.
6. Move orchestrator lifecycle mutations behind methods.
7. Tighten ACP runtime state after the protocol-flow comments and tests are in
   place.

Each step should land with focused regression coverage. The value comes from
shrinking the number of states reviewers must hold in their heads, not from
adding abstraction for its own sake.

> **Review note — suggested reordering.**
>
> `2 -> 1 -> 3a -> 3b -> 4 -> 7 -> 6 -> (drop or defer 5)`
>
> - **2 first**: Priority 2 catches a real SQLite-correctness bug (missing
>   tombstones from rollback) rather than hardening a defensive-hygiene shape.
>   Ship that correction ahead of response-shape cleanup.
> - **3 split into 3a + 3b**: land the four-line `next_mutation_stamp` reorder
>   (closing the active `bugs.md` entry) before the wrapper, so the bug fix
>   does not wait on the larger refactor.
> - **7 before 6**: the Option-heavy ACP state block is more contained than
>   the cross-cutting orchestrator lifecycle rework (which splits across
>   `AppState` coordinator methods and `OrchestratorInstance` state
>   transitions — see the Priority 6 review note).
> - **5 last or dropped**: no concrete bug to prevent; fold the delta-commit
>   coupling piece into Priority 3 instead.

## Candidates for Future Additions

Three additional hardening axes that fit the design principle but are not
currently on the plan. Treat them as backlog items rather than next-up
priorities.

### Unify pending-request registers

`SessionRecord` carries six parallel in-flight-request maps:
`pending_claude_approvals`, `pending_codex_approvals`,
`pending_codex_user_inputs`, `pending_codex_mcp_elicitations`,
`pending_codex_app_requests`, `pending_acp_approvals`. Each has its own
submission, cancellation, and preview-text path. A shared trait (or single
tagged enum) for "in-flight interaction request" would let registration,
expiration, cancellation-on-runtime-exit, and preview rendering live in one
place rather than six parallel copies. Downside: loses the per-agent typed
payload shape; needs careful trait design to keep the typed access.

### RuntimeToken guard wrapper

`RuntimeToken` is already a tagged enum, but the `_if_runtime_matches`
pattern is manually applied at every call site in
`src/turn_lifecycle.rs`, `src/shared_codex_mgr.rs`,
`src/session_identity.rs`, and `src/session_sync.rs`. A
`GuardedRuntime<'a, T>` type that can only hand out `&mut T` when the token
still matches would eliminate the "forgot to use the `_if_runtime_matches`
variant" foot-gun — the compile error would appear at the call site that
tried to mutate through a bare handle.

### Commit-variant typing

`commit_locked` vs `commit_delta_locked` vs `commit_session_created_locked`
is a compile-time-silent three-way choice. Pair each mutation helper with a
"mutation scope" marker type so, for example, a `DeltaMutation` can only be
resolved by `commit_delta_locked` with a published `DeltaEvent`. Natural
extension of Priority 3's `StampedSessionMut` (the wrapper can carry the
commit-variant marker) and a partial substitute for Priority 5 (the
revision used by the commit is the one the marker returns).
