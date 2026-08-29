# Engram Host Adapter

TermAl as the first host enforcement point for Engram's turn-admission
protocol: TermAl remains the actuator that starts, wakes, and stops sessions;
Engram becomes the decision point that says whether a session's next turn is
safe to run given what its peers have done, and what it must see first.

## Status

Phase 0 is implemented in the current TermAl changeset and is undergoing
conformance review; Phase 1 and later enforcement remain proposals. The
implementation targets the Engram host-private control channel that already ships (`engram control`,
JSON-lines: `session_bind`, `session_status`, `lease_acquire`,
`lease_release`, `turn_evaluate`, `turn_begin`, `turn_checkpoint`) and the
Engram control-plane brief's host integration contract. Engram references in
this document point at the Engram repository
(`docs/features/behavioral-control-plane.md`, `docs/features/local-work-system.md`,
`src/host.rs`); they are not TermAl documents.

Phase 0 below is the only implemented slice.

## Problem

TermAl routinely runs several agent sessions on one repository: a root
session, delegated reviewers and explorers, isolated-worktree workers, and
peers coordinating through durable mailboxes. Today three mechanisms keep
them from colliding: mailboxes (edge — messages that wake a session), the
coordination board (level — facts that never wake anyone), and human
approvals. None of them answers the question that actually decides whether
the next turn is safe: *has this session seen what its peers changed, does it
hold responsibility for what it is about to touch, and is anything it owes —
a checkpoint, a handoff, a contribution — still outstanding?*

Engram computes exactly that answer and returns it as a grant, a deferral, or
a typed refusal that names its own remedy. But Engram's own brief is explicit
that a tool an agent may *choose* to call is advisory, not control. Control
exists only where a host mediates the turn boundary — and TermAl owns that
boundary for every session it supervises. TermAl is therefore the natural
first host, and the one place where Engram's decisions can become real
without touching the agents themselves.

## Goals

- Every prompt TermAl dispatches to a bound session passes through one
  Engram turn evaluation, and every completed turn produces one checkpoint.
- Peer changes reach a session *inside the prompt it is about to run*, not
  through a tool it may forget to call.
- The integration is observable before it is enforcing: a shadow phase that
  records what Engram *would* have decided, with latency and false-refusal
  telemetry, before any prompt is ever withheld.
- The failure policy is explicit and never hangs a dispatch: Engram
  unreachable means the prompt proceeds and the session is visibly degraded.
- The adapter is one component with three insertion points, all of which
  already exist as single choke points in TermAl.
- **TermAl without Engram configured behaves exactly as TermAl before this
  adapter existed.** This is a tested invariant, on Greg's word, not a design
  statement. Concretely: no crate or build dependency on Engram — the
  transport is a subprocess speaking JSON-lines and nothing else; off by
  default, enabled only per project through `Project.engram`; when off,
  every hook is a single `Option` check — no child process, no transport
  call, no `EngramControl` card, no new persisted field written, no added
  latency; CI never requires the `engram` binary — the scripted fake and the
  fixture-script smoke test are the only test transports. Conformance S0
  asserts this and gates every other scenario.

## Non-goals for v1

- No action gating. Intercepting individual tool calls requires runtime
  mediation (Claude Code hooks, Codex app-server interception); the Engram
  brief defers this to a later phase and so does this one.
- No Engram work-graph UI in TermAl. Work items, claims, and leases stay in
  Engram's own surfaces (CLI/MCP) until Phase 0 data says a card is worth it.
- Engram never starts, pauses, wakes, or stops a TermAl session. Its verdicts
  only decide whether a dispatch proceeds now, later, or with an injected
  context page.
- TermAl never reimplements Engram policy. If Engram is absent the adapter
  degrades to shadow-off; it does not guess.
- Remote sessions (those whose `remote_session_target()` resolves) are out of
  scope; `dispatch_turn` proxies them before any adapter code runs, so this
  falls out of the insertion point rather than needing a check.
- Root (non-delegated) sessions are out of scope for Phase 0. See Rollout.

## Core Idea

Three insertion points, each already a single path in TermAl:

| Engram hook | TermAl location | What is already there |
| --- | --- | --- |
| `session_bind` | `StateInner::create_session` (`state_inner.rs`) — the path every delegation kind reaches through `create_read_only_delegation` (`delegations.rs`; despite its name it creates all modes and both write policies) — and `AppState::create_session` (`session_crud.rs`) for root sessions later | one creation path; isolated children carry their worktree root as `cwd` on the `DelegationRecord`; delegation creation also dispatches the child's first turn synchronously, so bind + first evaluate land inside that request |
| `turn_evaluate` | the queued-vs-dispatched decision in `dispatch_turn` (`turn_dispatch.rs`), **before** `start_turn_on_record` appends the prompt, marks the session `Active`, or spawns a runtime | **one** path for `PersistentClaude`, `PersistentCodex`, and `PersistentAcp`; remote sessions are proxied by `dispatch_turn`'s first statement and never reach it; this is the last point at which a prompt can still be queued instead of started — there is no un-start |
| inject → `turn_begin` | immediately before `deliver_turn_dispatch` (`api.rs`) sends the runtime `Prompt` command | the atomic recheck; a prompt is never sent on a grant that failed to begin |
| `turn_checkpoint` | `finish_turn_ok_if_runtime_matches` (`turn_lifecycle.rs`) **and every other turn termination**: `mark_turn_error`, Stop (`session_lifecycle.rs`), kill, runtime exit (`handle_runtime_exit_if_matches`) | per-runtime completion; `active_turn_file_changes` already records the files this turn touched (snapshot it before `finish_active_turn_file_change_tracking` arms its grace window) |

Everything else — grants, cursors, leases, refusal codes — lives in Engram.
The adapter translates TermAl events into those seven operations and
translates Engram's answer into one of three TermAl behaviours: dispatch as-is,
dispatch with an injected page, or requeue.

## Terminology

| TermAl | Engram | Notes |
| --- | --- | --- |
| session (`session_id`) | session | TermAl's id is the Engram `--session-id`; unique per spawned runtime |
| agent kind + session name | actor (`--actor-id`) | asserted identity; Engram records it as `asserted`, never authenticated |
| delegated child with write policy `IsolatedWorktree` | executor of a child work run in its own checkout | may hold claims and leases; same project store as the parent |
| delegated child with write policy `SharedWorktree { owned_paths }` | executor writing into the parent's checkout | the case where leases matter most; `owned_paths` is the natural seed for intent leases in Phase 1; watcher attribution is least reliable here. **Not creatable today**: `SharedWorktree` is `unreachable!` inside `create_read_only_delegation`, so Phase 0 yields no data for it; the only live writer policy is `IsolatedWorktree` |
| delegated child with write policy `ReadOnly` (any mode) | observe-only participant | never acquires a resource lease; requests only `observe` and `communicate` effects |
| parent of a bound child | root member, bound advisory, never evaluated in Phase 0 | gives the child's deltas a basis and lets Engram's root-member question be answered from data |
| project (`Project.root_path`) | project (`.engram-project` under `root_path`) | Engram resolves every worktree of the project to one store |
| `worktree_path` on an isolated delegation | same project, different checkout | Engram leases are keyed by project-relative subjects, so two worktrees conflict on the same logical path — the desired behaviour |
| mailbox | doorbell | a mailbox wake is a reason to run `turn_evaluate`; it is never the source of "what changed" |
| coordination board | not Engram | stays as is; level facts are not execution state |
| `SessionStatus::Approval` | human-only directive / `missing_authority` | derived state: an approval *suspends* a turn inside the runtime; answers arrive on dedicated routes, never through `dispatch_turn`, so the turn is neither a new turn nor complete |
| queued prompt source `User` / `Mailbox` / `Orchestrator` | turn intent | all three reach `dispatch_turn`, so all three are turns |

## Product Model

### The adapter

`EngramHostAdapter` is a TermAl component that owns, per bound session, one
`engram control` child process started with that session's
`--project-file`, `--actor-id`, and `--session-id`, plus the host-held
`routing_token` returned by `session_bind`. The token lives on the
`SessionRecord`, never in a transcript or a card. If Engram later multiplexes
sessions over one control process, the adapter changes; the protocol does not.

Binding happens at session creation for sessions in scope (see Rollout). The
bind declares `assurance: turn_gated` only once Phase 1 enforces; in Phase 0 it
declares `advisory`, because that is what is true.

**Migration shim, stated as such.** Engram's shipped host channel binds a
session to a *legacy task* by `external_ref`; it does not yet know
`root_execution_id`, `work_id`, or `run_id`, and the work graph reaches a
host session only through a task lookup. The `termal:delegation:<id>`
external reference is therefore a bridge, not the design. When Engram
exposes work-identity binding, the adapter binds `{root_execution_id,
work_id, run_id}` derived from the delegation ↔ work mapping and the
external reference is dropped. The transport trait and the single bind
struct keep that change local; Phase 1 must not ship on the shim. `mediated_effects` is
`[observe, communicate]` for read-only delegations and
`[observe, communicate, mutate_local]` for writers. `external_ref` is the
delegation's task reference when one exists, otherwise a TermAl-minted
reference for the session's root work.

### Before a turn

`dispatch_turn` today: reconcile never-woken mailbox notifications, revalidate
queued wake-ups, decide queued vs dispatched, build a `TurnDispatch`, and hand
it to `deliver_turn_dispatch`, which sends the runtime `Prompt` command.

With the adapter, at two distinct points:

**Lock discipline — a hard requirement.** `dispatch_turn` makes its
queued-vs-dispatched decision and mutates the record under the `StateMutex`.
No control call may run inside that critical section: a 250 ms round trip
there stalls every request and the SSE broadcaster for the whole process.
The adapter snapshots its inputs under the lock, calls Engram with the lock
released, then re-locks and validates that the session's dispatch generation
did not move before acting on the answer — the same fence pattern TermAl's
remote-authority work uses. A conformance test asserts the mutex is free
while a slow transport sleeps (Conformance, S6b).

The accepted generation travels with the runtime dispatch through
`turn_begin`. If Stop or a newer prompt supersedes it while begin is in flight,
the stale completion is dropped without converting the clean Stop to Error or
clearing the newer runtime. A genuine project-reset/persistence rejection still
runs the stale-begin cleanup. Any evaluated grant abandoned before begin — by
queue revalidation, failed runtime start, Stop, or runtime exit — advances the
generation and arms `rebind_required`, because Engram still owns that issued
grant.

1. `turn_evaluate` at the queued-vs-dispatched decision, **before** the
   record is mutated, with `purpose: ordinary`, `requested_effects` from the
   session's declared set, and an `intent_fingerprint` over `text`,
   `expanded_text`, attachment digests, and the source kind — except that a
   mailbox wake fingerprints the `(mailbox_id, through_sequence)` tuple,
   because its prompt text is synthesized. Deadline-bounded (see Failure
   policy). Phase 0 records the answer and proceeds; Phase 1 acts on it here,
   which is why the call must live at this point and not later.
2. On a **grant**: if `required_injections` is non-empty, prepend the
   delivered page to the prompt as a delimited context block (Phase 1) and
   call `turn_begin` with the grant id and the delivery tokens. Only after
   `turn_begin` succeeds is the `Prompt` command sent. The injected page is
   also recorded as a structured card so a human can see what the agent saw.
3. On a **defer**: requeue the prompt with TermAl's existing prompt queue and
   retry after `retry_after_ms` or on the next mailbox wake, whichever first.
   The queued card shows the deferral reason.
4. On a **refusal**: Phase 0 records it and dispatches anyway; Phase 1
   withholds only for the closed set listed under Rollout and otherwise falls
   back to recording. A refusal always carries `blocking_directives`; the
   adapter renders them on a control card and, for `audience: human`
   directives, surfaces them the way approvals are surfaced today.

`turn_begin` is the atomic recheck; a grant that expired or whose basis moved
between evaluate and begin is refused there, and the adapter treats that as a
fresh evaluate, not a failure.

The host scopes every `turn_begin` idempotency key to the concrete `grant_id`
and every `turn_checkpoint` key to both the `grant_id` and serialized
`next_intent`. Re-evaluation remains scoped to the stable intent fingerprint.
A key must never be reused when a replacement grant or checkpoint intent
changes the operation fingerprint: Engram persists refused operations and
rejects such reuse as `control_operation_idempotency_conflict`.

**Budget.** 250 ms cap per control call and a 600 ms overall dispatch budget
covering evaluate, begin, and at most one re-evaluate. Re-evaluate only on
begin refusals that mean the basis moved (`grant_expired`,
`policy_epoch_changed`, `task_admission_epoch_changed`, `delta_required`,
`stale_fence`), reusing the same `intent_fingerprint` so Engram binds the
retry as one intent. Anything else, a second refusal, or budget exhaustion
dispatches without a grant. A refusal outside that five-code set leaves its
grant `issued`; the adapter records the refusal, marks `rebind_required`, and
expires the orphan through a fresh bind before the next evaluate. Likewise,
an evaluate refusal with directive code `turn_already_open` is an ordinary
decision response, not a transport error, and arms the same repair path.

**A turn is checkpointed if and only if it was begun.** Engram's decision is
`Grant | Refuse | Defer`, and only a grant carries a `grant_id`; the adapter
never invents one. A dispatch after a deferral, refusal, timeout, or transport
error is a *dispatch without grant*: no `turn_begin`, no checkpoint; the card
keeps Engram's `decision` (`defer`, `refuse`, or `degraded`) and records
`dispatch: sent_without_grant` with the reason. The inverse obligation is hard:
once a grant is begun it **must** be checkpointed even if the runtime
dies, because `turn_evaluate` while a begun grant is open answers
`turn_already_open` and the session cannot take another turn until the open
one is closed.

### After a turn

`finish_turn_ok_if_runtime_matches` already knows the session, the runtime
token, and `active_turn_file_changes`. The adapter calls `turn_checkpoint`
with the grant id and a `next_intent` derived from TermAl state: `continue`
when prompts are queued, `wait` when the session can resume, and `exit` only
when it is being permanently removed. Every other way a turn ends must checkpoint too, or the
session stays `turn_open` in Engram forever — but the intent depends on
whether the TermAl session can resume. Verified against the real binary:
`exit` moves Engram's phase to `exited`, after which every evaluate answers
`session_exited` until a fresh `session_bind`. So: `wait` for Stop,
`mark_turn_error`, `fail_turn`, and runtime exit — the session record survives
and the user may resume it; `exit` only for kill, where the session is gone.
Delegation cancel currently leaves the child record stopped, so it uses
`wait` too. After any `exit`, mark the session `rebind_required`
so a later resume binds before it evaluates. (`TurnNextIntent` is
`continue | wait | exit`; there is no `abort`.) A turn that ends in `SessionStatus::Approval` is *not* checkpointed
until the approval resolves; the session stays `turn_open` on the Engram
side, which is accurate (see Approval turns below).

In Phase 1 the touched files become resource subjects on the checkpoint so
out-of-band writes are attributed — with a known limit: TermAl's attribution
is watcher-based containment, so a file event under a checkout shared by a
parent and a `SharedWorktree` child is attributed to both. Record it in
Phase 0; do not treat it as a trustworthy subject source until Phase 1
decides how to disambiguate.

Shared Codex app-server: one app-server serves every Codex session, so a
mid-run app-server restart resets all their runtimes at once. Re-bind-at-boot
must therefore also run on app-server restart, once per affected session,
never twice.

Restart. Two fields persist on the session row (`sessions.value_json`, via
`PersistedSessionRecord` with `#[serde(default)]` — no schema bump):
`engram_routing_token` and `engram_open_grant_id`, the latter set at
`turn_begin` and cleared at checkpoint. Both are absent from the row unless
the session was bound (`skip_serializing_if`), so a TermAl with the adapter
off writes exactly the rows it wrote before. The token is needed to talk to Engram
at all; the open grant id lets the adapter know *locally* that a turn is
unclosed even when Engram is unreachable at boot, so it can refuse to pretend
the session is clean. On TermAl boot, per in-scope session: (1)
`session_status` with the persisted token; (2) if Engram reports
`open_grant_id` — or TermAl's own row holds one — `turn_checkpoint(grant_id,
next_intent: wait)`; the runtime is gone, so the turn is over. Do **not**
condition this on TermAl's `SessionStatus`: interrupted sessions are normalized
to `Idle` during persisted-state recovery, so that status no longer preserves
whether Engram still has an open grant. Engram status plus the persisted local
grant id are the recovery authority.
Engram refuses to re-bind while a begun turn is open; (3) `session_bind` with
a fresh idempotency key and expect `sync_required`. If bind still refuses,
render a P0 card and leave the session unbound (shadow off) rather than
dispatch against inconsistent state. A `recoverable_grant` is treated the same
in Phase 0; redelivery of its page is Phase 1. No cached grant survives a
restart, by Engram's design.

Engram may also report an issued grant that never reached `turn_begin` as
`open_grant_id`. Such a grant cannot be checkpointed: both `wait` and `exit`
return `grant_scope_mismatch`. That specific recovery result means the host
must drop the stale local token and continue to a fresh `session_bind`, which
expires the issued grant. This exception does not apply to a begun grant;
begun grants must still checkpoint before bind.

Engram's planned control-hardening contract will make this distinction
explicit: `session_status` will report `open_grant_state: issued|begun`, a
fresh evaluate will atomically supersede an issued grant, and checkpointing an
issued grant will refuse with `grant_not_begun` plus recovery guidance. Until
those changes ship, Phase 0 deliberately keeps the issued -> attempted
checkpoint -> fresh-bind recovery above; it is compatible with both the
current and planned contract. A begun grant must never be replaced this way.

Boot ordering is a constraint, not a detail. `app_boot` runs
`reconcile_unread_mailbox_wakeups_after_boot`,
`reconcile_delegation_waits_after_boot`,
`resume_pending_orchestrator_transitions`, and
`dispatch_orphaned_workflow_prompts` — all of which can dispatch. Rebinds
therefore run **before** the mailbox-wakeup pass as a post-bind recovery
barrier. Recovery uses bounded batches of at most eight workers, each capped
at the 600 ms budget, so it cannot exhaust OS threads; later batches may extend
the barrier, but no mailbox/workflow prompt can overtake recovery. The same
procedure runs when the shared Codex app-server restarts mid-run.
Delegation children remain children during this recovery even if their
delegation row is temporarily missing: the durable parent-delegation marker
prevents fallback to a parent-shaped binding. Fatal `disabled_reason` state is
honoured for both child and parent targets.

Approval turns: a runtime that enters `SessionStatus::Approval` is still in
the same Engram turn. Checkpoint only at final runtime completion; no Engram
call at approval resolution. This is safe because checkpoint requires the
grant to be *begun* and checks no expiry — only unbegun grants expire.
Stop also resolves pending approvals (`cancel_pending_interaction_messages`
in the stop transition); the stopped session remains resumable, so that path
checkpoints with `wait` like the ordinary Stop transition.

### Identities

| Field | Value |
| --- | --- |
| `--session-id` | the TermAl child session id |
| `--actor-id` | a stable principal per agent kind, from an explicit table in the adapter — `Claude → termal:claude`, `Codex → termal:codex`, `Cursor → termal:acp:cursor`, `Gemini → termal:acp:gemini`, `OpenCode → termal:acp:opencode` — never the session name (mutable, non-unique) and never derived from the `Agent` enum's serde spelling; Engram matches work-authority grants on the actor id exactly, so per-session actor ids would need a grant per session |
| `external_ref` | `termal:delegation:<delegation id>` — Engram's task rendezvous key across sessions; derivable at boot from the persisted delegation row |
| `title` | the delegation title |
| `--project-file` | absolute `<root_path>/.engram-project` (`Project.root_path` is canonicalized on creation), also for isolated-worktree workers; `DelegationRecord.cwd` (the worktree root) is deliberately **not** used |
| `--home` | explicit, from `Project.engram.home` or `ENGRAM_HOME`; Engram has no default and a different home silently forks the store |
| `capability_map_revision` | 1; bump only when the mediated tool set changes |

Capability follows the **write policy**, not the mode: `IsolatedWorktree` and
`SharedWorktree` → writer, `mediated_effects: [observe, communicate,
mutate_local]`; `ReadOnly` → observe-only, `[observe, communicate]`.
`Reviewer` / `Explorer` / `Worker` are labels; the sandbox is the truth.

The control child process starts lazily at the first call that needs it
(bind is the first message either way) and is reaped on session kill or after
an idle timeout; the machine already carries dozens of per-session sidecars
and this one must not be resident for idle sessions.

### The control card

One structured card type, a typed `Message::EngramControl` variant persisted
through the same `value_json` path as every other message, rendered at
dispatch, at checkpoint, and at restart recovery. The UI renderer ships in
the same change as the variant — an older UI build renders nothing for an
unknown kind, and a shadow phase whose cards are invisible has no product.
Schema v1, owned by TermAl:
`schema_version: 1`; `stage: dispatch | checkpoint | restart`; `assurance`
(`advisory` in Phase 0); `decision: grant | defer | refuse | degraded` — what
Engram answered, or that it could not be asked; `dispatch: sent_on_grant |
sent_without_grant | queued` — what TermAl then did, so a refusal that was
dispatched anyway (every Phase 0 refusal) is legible as such;
`refusal_code?`; `defer_code?`; `grant_id?`;
`directives: [{directive_id, kind, audience, satisfaction}]`;
`delivered_range?: {from, to, head}`; `latency_ms: {evaluate?, begin?,
checkpoint?, total}`; `fail_mode: enforced | shadow | degraded`;
`repair_armed: boolean` — true when this outcome armed a fresh-bind repair;
`next_intent?`. Fields may be added; none removed or renamed without a
`schema_version` bump. In Phase 0 this card is the entire product: it is how
the team learns what enforcement would have done. Ordinary process logging
tagged with project and session ids is the only other output; the transcript
sqlite is the durable record and the source for Phase 0 aggregates.

## Failure Policy

| Condition | Phase 0 | Phase 1 |
| --- | --- | --- |
| Engram unreachable or over deadline | dispatch; card shows `degraded` | dispatch for reversible local work; card shows `degraded`; no injection |
| `control_unavailable`, `store_corrupt`, `unknown_control_schema`, `control_policy_missing` | dispatch; card; adapter disables itself for the session | withhold (non-overridable by Engram's own matrix); human card |
| refusal outside the closed set | dispatch; record | dispatch; record; count as false-refusal candidate |
| defer | dispatch; record | requeue |

Deadline defaults: 250 ms per call in Phase 0, 1 s in Phase 1, per project
setting. A hung dispatch is never acceptable; the adapter must time out, not
block.

**Enable-time vs run-time.** The opt-in lives on the thing it gates: an
`engram: Option<EngramProjectSettings>` field on `Project` — `enabled`,
`binary_path`, `home`, optional `work_authority_grant`, optional
`deadline_ms` — persisted through `app_state` `value_json` with a serde
default, never set for remote projects, and deleted with the project. There
is no per-project settings slot today; this adds one rather than keying a map
in the global `AppPreferences`. Enabling fails fast and names the reason
unless `<root_path>/.engram-project` exists and is non-empty, `binary_path`
and `home` are set explicitly, and `engram doctor` exits 0 **and reports a
`required=` control assurance that the assurance TermAl will declare covers**
(`advisory` in Phase 0). Engram's built-in policy requires `turn_gated`; a
session bound `advisory` against it is refused `control_assurance_insufficient`
on every evaluate, which would leave the shadow phase with nothing but
refusals. A stock store failing enablement for that exact reason is expected.
Engram has accepted a planned attributed, immutable control-policy update for
per-project `required_assurance`; the intended operator action is
`engram control-policy set-required-assurance <level> --authorized-by <actor>
--reason <text>` (final CLI spelling may be normalized). The update will bump
the policy epoch, so a burst of `policy_epoch_changed` re-evaluations after an
operator changes assurance is normal convergence, not a control-plane fault.
Until that Engram change ships, TermAl must report the refusal rather than
silently weakening either side's policy. `doctor` is a subprocess: run it in
the settings-save handler **before** taking the state lock, and treat a slow
doctor as a failed enable, never as a hang. After enabling, failures degrade
instead of blocking: a control child that exits or stops answering marks the
session `degraded`, dispatch proceeds, and binding retries with backoff (1 s,
5 s, 30 s, then every 60 s) and at every turn boundary, recovering silently.

**Operational guards.** Every guard, the disable sweep, and enable-time
resolution key on a session's *effective* project: an isolated-worktree child
carries no `project_id` on its own record and must be resolved through its
delegation to the parent's project, or the only writer policy escapes every
guard. A global kill switch (environment variable and project setting)
disables the adapter without a restart. Non-transport failures are not
retried at every turn: `store_corrupt`, `unknown_control_schema`,
`control_policy_missing`, and a response the adapter cannot parse disable
the adapter for that session until the next successful bind, and the card
says which; an invalid-token class (`control_session_token_mismatch`,
`control_session_not_bound`, `control_connection_superseded`) drops the
token and binds fresh. A per-session
circuit breaker turns the adapter off for that session after three
consecutive deadline misses or transport errors; the card says why, and the
session stays off until the next bind attempt succeeds. The blast radius of
this adapter is every delegated child's dispatch and finish path; these two
guards are what make the shadow phase safe to leave on.

## Security

- The routing token is host-held. It never appears in a prompt, a card, a
  transcript, or a mailbox message.
- Identity is asserted: TermAl's agent kind and session name become Engram's
  actor id, recorded at `asserted` assurance. Nothing in this brief claims
  more.
- Work-authority grants (`engram authority grant`) are installed by the
  operator per project; the grant hash is `Project.engram.work_authority_grant`.
  PATCH input is trimmed and must be a lowercase 64-character SHA-256 hash;
  blank, uppercase, short, or option-shaped values are rejected before any
  project mutation.
  `Project` is served on `/api/state` to every client, so `Project.engram`
  is visible to anyone who can see the project: `binary_path` and `home` are
  acceptable there; the grant hash is a capability under asserted identity and
  is stripped from client-facing project payloads.
- Disabling the adapter for a project attempts to terminate every begun grant
  under it: checkpoint first, then reap the control processes. The checkpoint
  is best-effort for this operator escape hatch: a deadline, transport error,
  or refusal produces a degraded checkpoint card and diagnostic log, but does
  not veto persisting the disabled setting or reaping the sidecar. A session
  whose checkpoint could not be confirmed retains only its durable routing
  token and open grant id with `rebind_required`; after re-enable the next
  dispatch performs status, a resumable `wait` checkpoint, and a fresh bind
  before evaluation. Sessions whose checkpoint succeeded clear their local
  binding/grant state normally. An enabled-to-enabled binary/home
  reconfiguration remains strict and returns a conflict if its checkpoint
  cannot be confirmed. Sessions that
  also get the Engram MCP server for the work protocol receive it through the
  per-session MCP configuration TermAl already writes at spawn, with
  `--work-authority-grant` fixed by TermAl, not by the agent.
- The MCP descriptor is invalidated from the same spawn-visible inputs that
  construct it: runtime eligibility, local-vs-remote placement, `binary_path`,
  `home`, and `work_authority_grant`. Binary/home changes, enablement, and grant
  rotation mark every affected local runtime (including delegation descendants)
  for rebuild at the next turn boundary, allowing the already-authorized turn
  to finish. Clearing a grant, disabling Engram, or deleting the project is an
  immediate revocation: after the settings/project mutation is durable and
  before process teardown, TermAl invokes `engram authority revoke` against the
  current project tuple and every distinct binary/home/grant tuple recorded on
  a still-live runtime. This runtime-only installed-descriptor record is made
  at the exact point the agent configuration is composed, is cleared with the
  runtime handle, is redacted from debug output, and is neither persisted nor
  exposed on the wire. It ensures that a later clear/disable/delete also
  revokes a stale descriptor left behind by an earlier deferred connection or
  grant rotation. Engram revalidates that grant inside every mutation
  transaction, so a residual MCP child cannot add, claim, note, or complete
  work after a successful revoke even when its agent interrupt is unconfirmed.
  Read-only MCP operations can remain available until Codex unloads the thread
  or the dedicated process exits. TermAl also fences runtime callbacks,
  checkpoints with exit intent, terminates or detaches the live
  Claude/Codex/ACP runtime, and resumes durable queued work only after a fresh
  runtime can be constructed and, on a connection reconfiguration, after the
  fresh Engram bind completes. Grant rotation remains deferred and does not
  irreversibly revoke the old hash during the already-authorized turn. The
  stale runtime set is fenced in the same commit as the settings change. Fence
  ownership also remains on the project, by generation, through off-lock
  authority revocation, runtime teardown, and any required fresh bind; an
  overlapping Engram settings mutation returns conflict instead of revoking a
  newer successful configuration. Runtime fence
  ownership carries the runtime token plus a monotonic generation, so a stale
  teardown completion cannot release a newer Stop/revocation owner; live model
  refresh is rejected while a turn or runtime-stop owner is active for the same
  reason. If an ordinary Stop or an earlier revocation already owns a session's
  fence, the mutation succeeds without waiting and normal snapshots retain the
  session ids whose revocation remains pending. Stop success leaves no runtime
  to revoke, while Stop failure transfers the existing fence directly to
  revocation cleanup and preserves the user's queue/pause policy. A cleanup
  failure does not roll back the already-persisted revocation: the API reports
  degraded cleanup (including any other pending session ids), the stale handle
  stays quarantined when a dedicated process has not confirmed exit, the
  session moves to Error, and automatic workflow dispatch remains blocked. A
  later explicit action can retry termination, and a natural process-exit
  callback releases the retained handle without resuming queued work. Shared
  Codex interruption errors are likewise surfaced as degraded after the
  logical session has been detached; they are never hidden by a post-detach
  no-op retry, and the shared app-server is preserved for unrelated sessions.
  If the irreversible grant-revoke command itself fails, teardown still runs
  and the API explicitly warns that the residual MCP child may continue
  mutating until it exits; the durable settings change is not rolled back.
  OpenCode revocation uses the same bounded graceful ACP cancel as an ordinary
  Stop before local process teardown. Runtime handles are
  process-local and are not persisted, so a TermAl host restart has the same
  pre-existing orphan-process limitation as other dedicated agent runtimes;
  restored records still require a fresh runtime. `root_path` is intentionally
  absent from the mutable fingerprint because project roots do not change after
  creation.
- An agent that can bypass TermAl's dispatch (a shell it spawns itself, a
  direct API call) is outside this boundary. The card says `advisory` or
  `turn_gated`; it never says more than the host actually mediates.

## Rollout

**Phase 0 — shadow.** Scope: delegated child sessions only — `IsolatedWorktree`
children as writers (and `SharedWorktree` once TermAl can create them),
`ReadOnly` children as observe-only — plus their parent, bound advisory and
never evaluated. Isolated children carry no `project_id` of their own; the
adapter resolves their project through the delegation parent for binding and
must do the same for every guard. Opt-in per project.
The adapter runs the full protocol from the pre-mutation decision point,
withholds nothing, and records every decision on the control card. Success is
measured, not asserted: p99 evaluate+begin latency off-lock, false-refusal
rate, mediation coverage, and the base rate of real conflicts across two weeks
of this team's own multi-session work.

**Phase 1 — turn-gated delivery.** Inject the delta and pinned context into
the prompt; withhold only for the closed set: `pinned_contradiction`,
`pinned_budget_exceeded`, `recovery_required` (unreconciled lease recovery),
`lifecycle_hold` (completion or finalization pending), and the four
non-overridable conditions above. Everything else stays record-only until
Phase 0 telemetry justifies widening. Root sessions join here if Phase 0 shows
no false refusals on children.

**Later — action gating.** TermAl already sends each Claude session an
`initialize` control request carrying an empty `hooks` map at spawn; a
`PreToolUse` hook to a thin Engram client can be injected there and is the
obvious path to `action_gated` assurance for Claude sessions. Codex receives
its MCP configuration in the thread-start request and ACP agents in
`session/new`; both need interception at the app-server or protocol layer.
Not before Phase 1 data.

## Conformance

TermAl tests do not run real agents: runtimes are in-process fakes with an
mpsc command channel the test drains, events are injected through the same
functions the real runtimes call, and restart is a fresh `AppState` over the
same SQLite. The adapter therefore exposes its transport as a trait: the real
JSON-lines child process, a `cfg(test)` scripted fake for exact failure
sequences, and a stateful fake that models the grant lifecycle exercised by
the adapter. Adapter-level conformance scenarios run against those in-process
fakes. The fixture-script process is a transport/protocol conformance seam,
not an adapter-level execution path; its smoke tests hand-drive requests to
cover spawn, EOF, kill, timeout, stale-begin replacement, and orphaned issued
grant recovery. Both fakes model `turn_already_open` as a refusal decision,
preserve issued grants across non-expiring begin refusals, and reject bind over
a begun grant as `invalid_control_session`, matching the real control binary.
The Phase 0
scenario set (S0–S14) is maintained by the inspecting session and driven
against each delivery. **S0 runs first, and an S0 failure is a NO-GO
regardless of S1–S14.** The minimum the set must cover:

- **S0 — off means off.** With no `Project.engram`, the full create →
  dispatch → finish → restart cycle against the scripted fake produces zero
  transport calls, zero spawned control processes, no `EngramControl`
  messages, no new fields on any persisted row, and transcripts byte-identical
  to a build without the adapter;
- every prompt source produces exactly one `turn_evaluate` and, when
  dispatched, one `turn_begin` — exercised explicitly for `User`, `Mailbox`
  (a committed mailbox message wake), and `Orchestrator` (a delegation-wait
  resume); a `Queued` outcome produces zero calls;
- a grant whose project-policy or admission epoch moved between evaluate and
  begin is refused at begin and re-evaluated, never dispatched on the stale
  grant;
- **S13 — issued but never begun.** If TermAl invalidates a dispatch after
  evaluate issues a grant but before begin succeeds, the row is marked
  `rebind_required`; the next turn observes the open grant, receives
  `grant_scope_mismatch` from checkpoint, performs exactly one fresh bind,
  then evaluates and begins normally. The same repair is required after a
  non-expiring begin refusal such as `lifecycle_hold`, and when a later
  evaluate reports `turn_already_open` as a refusal directive;
- a routing token replayed from another session is refused, asserted
  explicitly across two bound sessions, not implied;
- a mailbox wake arriving while the session is `checkpoint_required` — in
  Phase 0, which never withholds — produces an evaluate recorded as
  `refuse` with code `checkpoint_required` and a dispatch marked
  `sent_without_grant`; queuing instead of dispatching is the Phase 1 form of
  this scenario;
- TermAl restart mid-turn checkpoints the recovered open grant with `wait`,
  binds fresh at `sync_required`, and evaluates the next ordinary turn;
- Engram unreachable: the prompt still dispatches within the deadline and the
  card shows `degraded`;
- read-only delegations never request `mutate_local` and never acquire a
  lease;
- Stop, delegation cancel, `mark_turn_error`, `fail_turn`, and runtime exit
  each produce exactly one checkpoint with `next_intent: wait`, and a
  follow-up turn afterwards is granted; kill produces exactly one checkpoint
  with `next_intent: exit` and marks the session `rebind_required`; no session
  is left `turn_open`;
- **S14 — disable remains an escape hatch.** Disabling `Project.engram` while
  a grant is begun attempts its checkpoint
  before the control process is reaped; even when every checkpoint reaches a
  deadline or is refused, the PATCH succeeds, every project sidecar is reaped,
  and the disabled setting is durable. Failed-checkpoint sessions preserve the
  token/open-grant recovery tuple and arm repair; after re-enable, the first
  dispatch checkpoints that same open grant with `wait`, binds fresh, and is
  granted rather than remaining at `turn_already_open`;
- changing `binary_path` or `home` while enabled resets every bound session
  against the old connection (token dropped, fresh bind); an invalid-token
  error class from Engram drops the token and binds fresh instead of backing
  off forever;
- a remote-target session is never bound or evaluated; `dispatch_turn` still
  proxies it;
- a pending approval produces no checkpoint, answering it produces no
  evaluate, and completion after the answer yields exactly one checkpoint;
- a shared Codex app-server restart re-binds every bound Codex session once,
  with no duplicate binds;
- while a transport request is blocked on a test hand-off, the `StateMutex` is
  free;
- crash-restart with a persisted open grant id: boot checkpoints it with
  `next_intent: wait`, rebinds exactly once, and the first dispatch is
  evaluated normally.

## Open Questions

1. **One control process per session or per project.** `engram control`
   fixes actor and session by process arguments, which implies one child
   process per bound session. That is simple and isolates failures, but
   costs a process per session. Multiplexing is an Engram-side change to
   raise with the Engram maintainers only if Phase 0 shows the cost matters.
2. **Where the injected page renders.** A persistent Claude process, the
   shared Codex app-server, and ACP runtimes accept prompts differently; a
   prompt prefix is the portable choice, a system-level preamble may be
   cleaner where a runtime supports it. Phase 0 does not inject, so this is a
   Phase 1 decision informed by which runtimes actually show up in the data.
3. **Which TermAl session is the Engram "root member".** A delegation tree
   maps naturally onto Engram's root execution with child runs; a peer
   mailbox group does not. Phase 0 binds children to their parent's root
   reference and leaves peer groups unbound.
4. **Approvals as directives.** Whether a pending Engram human-only directive
   should reuse TermAl's approval surface or get its own card. Phase 0 renders
   a card; Phase 1 decides.

## References

- Engram: `docs/features/behavioral-control-plane.md` — host integration
  contract, planned interfaces, failure matrix, control assurance levels.
- Engram: `docs/features/local-work-system.md` — the six-operation work
  protocol and the Host Enforcement SDK hook list.
- Engram: `src/host.rs` — the shipped JSON-lines request and response shapes.
- TermAl: [Durable agent mailboxes](agent-mailboxes.md) — the edge/level split
  this adapter preserves.
- TermAl: [Agent delegation sessions](agent-delegation-sessions.md) — the
  delegation modes and write policies mapped above.
